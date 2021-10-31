#![allow(dead_code)]

use std::time::Duration;

use ahash::RandomState;
use dashmap::{mapref::one::RefMut, DashMap};
use harmony_rust_sdk::api::{
    exports::hrpc::exports::futures_util::TryFutureExt,
    harmonytypes::Token,
    sync::{event::*, postbox_service_client::PostboxServiceClient, *},
};
use hyper::{http::HeaderValue, Uri};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
    config::FederationConfig,
    key::{self, Manager as KeyManager},
};

use super::{chat::ChatTree, get_time_secs, http, prelude::*};
use db::sync::*;

pub mod notify_new_id;
pub mod pull;
pub mod push;

pub struct EventDispatch {
    pub host: SmolStr,
    pub event: Event,
}

struct Clients(DashMap<SmolStr, PostboxServiceClient, RandomState>);

impl Clients {
    fn get_client(&self, host: SmolStr) -> RefMut<'_, SmolStr, PostboxServiceClient, RandomState> {
        self.0.entry(host.clone()).or_insert_with(|| {
            // TODO: Handle url parsing error
            let host_url: Uri = host.parse().unwrap();

            PostboxServiceClient::new(host_url).unwrap()
        })
    }
}

#[derive(Clone)]
pub struct SyncServer {
    chat_tree: ChatTree,
    sync_tree: ArcTree,
    keys_manager: Option<Arc<KeyManager>>,
    federation_config: Option<FederationConfig>,
    host: String,
    disable_ratelimits: bool,
}

impl SyncServer {
    pub fn new(deps: &Dependencies, mut dispatch_rx: UnboundedReceiver<EventDispatch>) -> Self {
        let sync = Self {
            chat_tree: deps.chat_tree.clone(),
            sync_tree: deps.sync_tree.clone(),
            keys_manager: deps.key_manager.clone(),
            federation_config: deps.config.federation.clone(),
            host: deps.config.host.clone(),
            disable_ratelimits: deps.config.policy.disable_ratelimits,
        };
        let sync2 = sync.clone();
        let clients = Clients(DashMap::default());

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = async {
                        let hosts = sync2.sync_tree.scan_prefix(HOST_PREFIX).flat_map(|res| {
                            let key = match res {
                                Ok((key, _)) => key,
                                Err(err) => {
                                    let err = ServerError::DbError(err);
                                    tracing::error!("error occured while getting hosts for sync: {}", err);
                                    return None;
                                }
                            };
                            let (_, host_raw) = key.split_at(HOST_PREFIX.len());
                            let host = unsafe { std::str::from_utf8_unchecked(host_raw) };
                            Some(SmolStr::new(host))
                        });

                        for host in hosts {
                            if sync2.is_host_allowed(&host).is_ok() {
                                let mut client = clients.get_client(host.clone());
                                if let Ok(queue) = sync2
                                    .generate_request(PullRequest {})
                                    .map_err(|_| ())
                                    .and_then(|req| {
                                        client.pull(req).map_err(|_| ())
                                    })
                                    .await
                                {
                                    for event in queue.event_queue {
                                        if let Err(err) = sync2.push_logic(&host, event) {
                                            tracing::error!("error while executing sync event: {}", err);
                                        }
                                    }
                                }
                            }
                        }

                        tokio::time::sleep(Duration::from_secs(60)).await;
                    } => {}
                    _ = async {
                        while let Some(EventDispatch { host, event }) = dispatch_rx.recv().await {
                            if sync2.is_host_allowed(&host).is_ok() {
                                match sync2.get_event_queue(&host) {
                                    Ok(queue) => {
                                        if !queue.event_queue.is_empty() {
                                            if let Err(err) = sync2.push_to_event_queue(&host, queue, event) {
                                                tracing::error!("error while pushing to event queue: {}", err);
                                            }
                                            continue;
                                        }

                                        let mut client = clients.get_client(host.clone());
                                        let mut push_result = sync2
                                            .generate_request(PushRequest { event: Some(event.clone()) })
                                            .map_err(|_| ())
                                            .and_then(|req| {
                                                client.push(req).map_err(|_| ())
                                            })
                                            .await;
                                        let mut try_count = 0;
                                        while try_count < 5 && push_result.is_err() {
                                            push_result = sync2
                                                .generate_request(PushRequest { event: Some(event.clone()) })
                                                .map_err(|_| ())
                                                .and_then(|req| {
                                                    client.push(req).map_err(|_| ())
                                                })
                                                .await;
                                            try_count += 1;
                                        }

                                        if push_result.is_err() {
                                            if let Err(err) = sync2.push_to_event_queue(&host, queue, event) {
                                                tracing::error!("error while pushing to event queue: {}", err);
                                            }
                                        }
                                    }
                                    Err(err) => tracing::error!("error occured while getting event queue: {}", err),
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
            server_id: self.host.clone(),
            time: get_time_secs(),
        };

        let token = self.keys_manager()?.generate_token(data).await?;
        let token = rkyv_ser(&token);

        let mut req = Request::new(msg);
        req.header_map_mut()
            .insert(http::header::AUTHORIZATION, unsafe {
                HeaderValue::from_maybe_shared_unchecked(token)
            });

        Ok(req)
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
        let maybe_auth = request.header_map().get(&http::header::AUTHORIZATION);

        if let Some(auth) = maybe_auth.map(|h| h.as_bytes()) {
            let token = Token::decode(auth).map_err(|_| ServerError::InvalidToken)?;

            let AuthData { server_id, time } = AuthData::decode(token.data.as_slice())
                .map_err(|_| ServerError::InvalidTokenData)?;

            self.is_host_allowed(&server_id)?;

            let cur_time = get_time_secs();
            // Check time variance (1 minute)
            if time < cur_time + 30 && time > cur_time - 30 {
                let keys_manager = self.keys_manager()?;

                let host: SmolStr = server_id.into();
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

    fn push_logic(&self, host: &str, event: Event) -> ServerResult<()> {
        if let Some(kind) = event.kind {
            match kind {
                Kind::UserRemovedFromGuild(UserRemovedFromGuild { user_id, guild_id }) => {
                    self.chat_tree
                        .remove_guild_from_guild_list(user_id, guild_id, host)?;
                }
                Kind::UserAddedToGuild(UserAddedToGuild { user_id, guild_id }) => {
                    self.chat_tree
                        .add_guild_to_guild_list(user_id, guild_id, host)?;
                }
                Kind::UserInvited(_) => todo!(),
                Kind::UserRejectedInvite(_) => todo!(),
            }
        }
        Ok(())
    }

    fn get_event_queue(&self, host: &str) -> Result<PullResponse, ServerError> {
        let key = make_host_key(host);
        let queue = self
            .sync_tree
            .get(&key)?
            .map_or_else(PullResponse::default, |val| {
                rkyv_arch::<PullResponse>(&val)
                    .deserialize(&mut rkyv::Infallible)
                    .unwrap()
            });
        self.sync_tree.remove(&key)?;
        Ok(queue)
    }

    fn push_to_event_queue(
        &self,
        host: &str,
        mut queue: PullResponse,
        event: Event,
    ) -> Result<(), ServerError> {
        // TODO: this is a waste, find a way to optimize this
        queue.event_queue.push(event);
        let buf = rkyv_ser(&queue);
        self.sync_tree.insert(&make_host_key(host), buf.as_ref())?;
        Ok(())
    }
}

impl postbox_service_server::PostboxService for SyncServer {
    impl_unary_handlers! {
        pull, PullRequest, PullResponse;
        push, PushRequest, PushResponse;
        notify_new_id, NotifyNewIdRequest, NotifyNewIdResponse;
    }
}