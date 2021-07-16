#![allow(dead_code)]

use std::collections::HashMap;

use ahash::RandomState;
use harmony_rust_sdk::api::{
    exports::{
        hrpc::{async_trait, encode_protobuf_message, Request},
        prost::Message,
    },
    harmonytypes::Token,
    sync::{event::*, postbox_service_client::PostboxServiceClient, *},
};
use prost::bytes::BytesMut;
use reqwest::Url;
use smol_str::SmolStr;
use tokio::sync::mpsc::UnboundedReceiver;
use triomphe::Arc;

use crate::{
    db::{sync::*, ArcTree},
    impls::{chat::ChatTree, get_time_secs, http},
    key::{self, Manager as KeyManager},
    ServerError,
};

pub struct EventDispatch {
    pub host: SmolStr,
    pub event: Event,
}

struct Clients(HashMap<SmolStr, PostboxServiceClient, RandomState>);

impl Clients {
    fn get_client(&mut self, host: SmolStr) -> &mut PostboxServiceClient {
        self.0.entry(host.clone()).or_insert_with(|| {
            let http = reqwest::Client::new(); // each server gets its own http client
            let host_url: Url = host.parse().unwrap();

            PostboxServiceClient::new(http, host_url).unwrap()
        })
    }
}

#[derive(Clone)]
pub struct SyncServer {
    chat_tree: ChatTree,
    sync_tree: ArcTree,
    keys_manager: Arc<KeyManager>,
}

impl SyncServer {
    pub fn new(
        chat_tree: ChatTree,
        sync_tree: ArcTree,
        keys_manager: Arc<KeyManager>,
        mut dispatch_rx: UnboundedReceiver<EventDispatch>,
    ) -> Self {
        let sync = Self {
            chat_tree,
            sync_tree,
            keys_manager,
        };
        let sync2 = sync.clone();

        tokio::spawn(async move {
            let mut clients = Clients(HashMap::default());

            const PREFIX: &[u8] = HOST_PREFIX;
            let hosts = sync2.sync_tree.scan_prefix(PREFIX).map(|res| {
                let (key, _) = res.unwrap();
                let (_, host_raw) = key.split_at(PREFIX.len());
                let host = unsafe { std::str::from_utf8_unchecked(host_raw) };
                SmolStr::new(host)
            });

            for host in hosts {
                if let Ok(queue) = clients.get_client(host.clone()).pull(()).await {
                    for event in queue.events {
                        sync2.push_logic(&host, event);
                    }
                }
            }

            loop {
                while let Some(EventDispatch { host, event }) = dispatch_rx.recv().await {
                    let mut push_result =
                        clients.get_client(host.clone()).push(event.clone()).await;
                    let mut try_count = 0;
                    while try_count < 5 && push_result.is_err() {
                        push_result = clients.get_client(host.clone()).push(event.clone()).await;
                        try_count += 1;
                    }

                    if push_result.is_err() {
                        // TODO: this is a waste, find a way to optimize this
                        let mut queue = sync2.get_event_queue(&host);
                        queue.events.push(event);
                        let mut buf = BytesMut::new();
                        encode_protobuf_message(&mut buf, queue);
                        sync2
                            .sync_tree
                            .insert(&make_host_key(&host), buf.as_ref())
                            .unwrap();
                    }
                }
            }
        });
        sync
    }

    async fn auth<T>(&self, request: &Request<T>) -> Result<SmolStr, ServerError> {
        let maybe_auth = request.get_header(&http::header::AUTHORIZATION);

        if let Some(auth) = maybe_auth.map(|h| h.as_bytes()) {
            let token = Token::decode(auth).map_err(|_| ServerError::InvalidToken)?;

            let AuthData { host, time } = AuthData::decode(token.data.as_slice())
                .map_err(|_| ServerError::InvalidTokenData)?;

            let cur_time = get_time_secs();
            // Check time variance (1 minute)
            if time < cur_time + 30 && time > cur_time - 30 {
                let host: SmolStr = host.into();
                let pubkey = self.keys_manager.get_key(host.clone()).await?;

                return key::verify_token(&token, &pubkey).map(|_| host);
            }
        }

        Err(ServerError::FailedToAuthSync)
    }

    fn push_logic(&self, host: &str, event: Event) {
        if let Some(kind) = event.kind {
            match kind {
                Kind::UserRemovedFromGuild(UserRemovedFromGuild { user_id, guild_id }) => {
                    self.chat_tree
                        .remove_guild_from_guild_list(user_id, guild_id, host);
                }
                Kind::UserAddedToGuild(UserAddedToGuild { user_id, guild_id }) => {
                    self.chat_tree
                        .add_guild_to_guild_list(user_id, guild_id, host);
                }
            }
        }
    }

    fn get_event_queue(&self, host: &str) -> EventQueue {
        self.sync_tree
            .get(&make_host_key(host))
            .unwrap()
            .map_or_else(EventQueue::default, |val| {
                EventQueue::decode(val.as_ref()).unwrap()
            })
    }
}

#[async_trait]
impl postbox_service_server::PostboxService for SyncServer {
    type Error = ServerError;

    async fn pull(&self, request: Request<()>) -> Result<EventQueue, Self::Error> {
        let host = self.auth(&request).await?;
        let queue = self.get_event_queue(&host);
        Ok(queue)
    }

    async fn push(&self, request: Request<Event>) -> Result<(), Self::Error> {
        let host = self.auth(&request).await?;
        let key = make_host_key(&host);
        if !self.sync_tree.contains_key(&key).unwrap() {
            self.sync_tree.insert(&key, &[]).unwrap();
        }
        self.push_logic(&host, request.into_parts().0);
        Ok(())
    }
}
