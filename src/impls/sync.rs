#![allow(dead_code)]

use std::time::Duration;

use ahash::RandomState;
use dashmap::{mapref::one::RefMut, DashMap};
use harmony_rust_sdk::api::{
    exports::{
        hrpc::{async_trait, encode_protobuf_message, futures_util::TryFutureExt, Request},
        prost::Message,
    },
    harmonytypes::Token,
    sync::{event::*, postbox_service_client::PostboxServiceClient, *},
};
use reqwest::{header::HeaderValue, Url};
use smol_str::SmolStr;
use tokio::sync::mpsc::UnboundedReceiver;
use triomphe::Arc;

use crate::{
    config::FederationConfig,
    db::{sync::*, ArcTree},
    impls::{chat::ChatTree, get_time_secs, http},
    key::{self, Manager as KeyManager},
    ServerError,
};

pub struct EventDispatch {
    pub host: SmolStr,
    pub event: Event,
}

struct Clients(DashMap<SmolStr, PostboxServiceClient, RandomState>);

impl Clients {
    fn get_client(&self, host: SmolStr) -> RefMut<'_, SmolStr, PostboxServiceClient, RandomState> {
        self.0.entry(host.clone()).or_insert_with(|| {
            let http = reqwest::Client::new(); // each server gets its own http client
                                               // TODO: Handle url parsing error
            let host_url: Url = host.parse().unwrap();

            PostboxServiceClient::new(http, host_url).unwrap()
        })
    }
}

#[derive(Clone)]
pub struct SyncServer {
    chat_tree: ChatTree,
    sync_tree: ArcTree,
    keys_manager: Option<Arc<KeyManager>>,
    federation_config: Option<Arc<FederationConfig>>,
    host: String,
}

impl SyncServer {
    pub fn new(
        chat_tree: ChatTree,
        sync_tree: ArcTree,
        keys_manager: Option<Arc<KeyManager>>,
        mut dispatch_rx: UnboundedReceiver<EventDispatch>,
        federation_config: Option<Arc<FederationConfig>>,
        host: String,
    ) -> Self {
        let sync = Self {
            chat_tree,
            sync_tree,
            keys_manager,
            federation_config,
            host,
        };
        let sync2 = sync.clone();
        let clients = Clients(DashMap::default());

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = async {
                        let hosts = sync2.sync_tree.scan_prefix(HOST_PREFIX).map(|res| {
                            let (key, _) = res.unwrap();
                            let (_, host_raw) = key.split_at(HOST_PREFIX.len());
                            let host = unsafe { std::str::from_utf8_unchecked(host_raw) };
                            SmolStr::new(host)
                        });

                        for host in hosts {
                            if sync2.is_host_allowed(&host).is_ok() {
                                let mut client = clients.get_client(host.clone());
                                if let Ok(queue) = sync2
                                    .generate_request(())
                                    .map_err(|_| ())
                                    .and_then(|req| {
                                        client.pull(req).map_err(|_| ())
                                    })
                                    .await
                                {
                                    for event in queue.events {
                                        sync2.push_logic(&host, event);
                                    }
                                }
                            }
                        }

                        tokio::time::sleep(Duration::from_secs(60)).await;
                    } => {}
                    _ = async {
                        while let Some(EventDispatch { host, event }) = dispatch_rx.recv().await {
                            if sync2.is_host_allowed(&host).is_ok() {
                                let queue = sync2.get_event_queue(&host);
                                if !queue.events.is_empty() {
                                    sync2.push_to_event_queue(&host, queue, event);
                                    continue;
                                }

                                let mut client = clients.get_client(host.clone());
                                let mut push_result = sync2
                                    .generate_request(event.clone())
                                    .map_err(|_| ())
                                    .and_then(|req| {
                                        client.push(req).map_err(|_| ())
                                    })
                                    .await;
                                let mut try_count = 0;
                                while try_count < 5 && push_result.is_err() {
                                    push_result = sync2
                                        .generate_request(event.clone())
                                        .map_err(|_| ())
                                        .and_then(|req| {
                                            client.push(req).map_err(|_| ())
                                        })
                                        .await;
                                    try_count += 1;
                                }

                                if push_result.is_err() {
                                    sync2.push_to_event_queue(&host, queue, event);
                                }
                            }
                        }
                    } => {}
                }
            }
        });

        sync
    }

    async fn generate_request<Msg: Message>(&self, msg: Msg) -> Result<Request<Msg>, ServerError> {
        let data = AuthData {
            host: self.host.clone(),
            time: get_time_secs(),
        };

        let token = self.keys_manager()?.generate_token(data).await?;
        let token = encode_protobuf_message(token).freeze();

        Ok(
            Request::new(msg).header(http::header::AUTHORIZATION, unsafe {
                HeaderValue::from_maybe_shared_unchecked(token)
            }),
        )
    }

    fn keys_manager(&self) -> Result<&Arc<KeyManager>, ServerError> {
        self.keys_manager
            .as_ref()
            .ok_or(ServerError::FederationDisabled)
    }

    fn is_host_allowed(&self, host: &str) -> Result<(), ServerError> {
        self.federation_config
            .as_ref()
            .map_or(Err(ServerError::FederationDisabled), |conf| {
                conf.is_host_allowed(host)
            })
    }

    async fn auth<T>(&self, request: &Request<T>) -> Result<SmolStr, ServerError> {
        let maybe_auth = request.get_header(&http::header::AUTHORIZATION);

        if let Some(auth) = maybe_auth.map(|h| h.as_bytes()) {
            let token = Token::decode(auth).map_err(|_| ServerError::InvalidToken)?;

            let AuthData { host, time } = AuthData::decode(token.data.as_slice())
                .map_err(|_| ServerError::InvalidTokenData)?;

            self.is_host_allowed(&host)?;

            let cur_time = get_time_secs();
            // Check time variance (1 minute)
            if time < cur_time + 30 && time > cur_time - 30 {
                let keys_manager = self.keys_manager()?;

                let host: SmolStr = host.into();
                let get_key = || keys_manager.get_key(host.clone());
                let mut pubkey = get_key().await?;

                let verify = |pubkey| key::verify_token(&token, &pubkey).map(|_| host.clone());
                // Fetch pubkey if the verification fails, it might have changed
                if matches!(verify(pubkey), Err(ServerError::CouldntVerifyTokenData)) {
                    keys_manager.invalidate_key(&host);
                    pubkey = get_key().await?;
                }

                return verify(pubkey);
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
        let key = make_host_key(host);
        let queue = self
            .sync_tree
            .get(&key)
            .unwrap()
            .map_or_else(EventQueue::default, |val| {
                EventQueue::decode(val.as_ref()).unwrap()
            });
        self.sync_tree.remove(&key).unwrap();
        queue
    }

    fn push_to_event_queue(&self, host: &str, mut queue: EventQueue, event: Event) {
        // TODO: this is a waste, find a way to optimize this
        queue.events.push(event);
        let buf = encode_protobuf_message(queue);
        self.sync_tree
            .insert(&make_host_key(host), buf.as_ref())
            .unwrap();
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
