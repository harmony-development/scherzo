#![allow(dead_code)]

use std::collections::HashMap;

use super::{chat::ChatTree, keys_manager::KeysManager, *};

use ahash::RandomState;
use harmony_rust_sdk::api::{
    exports::{
        hrpc::{async_trait, return_print, server::Socket, Request},
        prost::Message,
    },
    harmonytypes::Token,
    sync::{event::*, postbox_service_client::PostboxServiceClient, *},
};
use reqwest::Url;
use tokio::sync::{
    broadcast::{self, Sender as BroadcastSend},
    mpsc::UnboundedReceiver,
};

use triomphe::Arc;

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
    keys_manager: Arc<KeysManager>,
    pull_queue_broadcast: BroadcastSend<Arc<EventDispatch>>,
}

impl SyncServer {
    pub fn new(
        chat_tree: ChatTree,
        keys_manager: Arc<KeysManager>,
        mut dispatch_rx: UnboundedReceiver<EventDispatch>,
    ) -> Self {
        let sync = Self {
            chat_tree,
            keys_manager,
            pull_queue_broadcast: broadcast::channel(1000).0,
        };
        let sync2 = sync.clone();
        tokio::spawn(async move {
            let mut clients = Clients(HashMap::default());

            loop {
                while let Some(dispatch) = dispatch_rx.recv().await {
                    if clients
                        .get_client(dispatch.host.clone())
                        .push(dispatch.event.clone())
                        .await
                        .is_err()
                    {
                        drop(sync2.pull_queue_broadcast.send(Arc::new(dispatch)));
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

                return verify_token(&token, &pubkey).map(|_| host);
            }
        }

        Err(ServerError::FailedToAuthSync)
    }
}

#[async_trait]
impl postbox_service_server::PostboxService for SyncServer {
    type Error = ServerError;

    type PullValidationType = SmolStr;

    async fn pull_validation(&self, request: Request<Option<Ack>>) -> Result<SmolStr, Self::Error> {
        self.auth(&request).await
    }

    async fn pull(&self, host: SmolStr, mut socket: Socket<Ack, Syn>) {
        let mut id = 0;
        let mut rx = self.pull_queue_broadcast.subscribe();

        loop {
            let mut value = None;
            while value.is_none() {
                value = rx.recv().await.ok();
            }
            let value = unsafe { value.unwrap_unchecked() };
            if value.host == host {
                return_print!(
                    socket
                        .send_message(Syn {
                            event: Some(value.event.clone()),
                            event_id: id,
                        })
                        .await
                );

                return_print!(socket.receive_message().await, |ack| {
                    if Some(id) != ack.map(|ack| ack.event_id) {
                        return;
                    }
                });

                id += 1;
            }
        }
    }

    async fn push(&self, request: Request<Event>) -> Result<(), Self::Error> {
        let host = self.auth(&request).await?;
        if let Some(kind) = request.into_parts().0.kind {
            match kind {
                Kind::UserRemovedFromGuild(UserRemovedFromGuild { user_id, guild_id }) => {
                    self.chat_tree
                        .remove_guild_from_guild_list(user_id, guild_id, &host);
                }
                Kind::UserAddedToGuild(UserAddedToGuild { user_id, guild_id }) => {
                    self.chat_tree
                        .add_guild_to_guild_list(user_id, guild_id, &host);
                }
            }
        }
        Ok(())
    }
}
