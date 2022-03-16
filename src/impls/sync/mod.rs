#![allow(dead_code)]

use std::time::Duration;

use crate::api::{
    harmonytypes::Token,
    sync::{event::*, postbox_service_client::PostboxServiceClient, *},
};
use ahash::RandomState;
use dashmap::{mapref::one::RefMut, DashMap};
use hrpc::exports::futures_util::TryFutureExt;
use hrpc::{client::transport::http::Hyper, encode::encode_protobuf_message};
use hyper::{http::HeaderValue, Uri};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{error, Instrument};

use crate::key::{self, Manager as KeyManager};

use super::{http, prelude::*};
use db::sync::*;

pub mod notify_new_id;
pub mod pull;
pub mod push;

pub struct EventDispatch {
    pub host: SmolStr,
    pub event: Event,
}

#[derive(Clone)]
pub struct Clients(Arc<DashMap<SmolStr, PostboxServiceClient<Hyper>, RandomState>>);

impl Clients {
    fn new() -> Self {
        Self(Arc::new(DashMap::default()))
    }

    fn get_client(
        &self,
        host: SmolStr,
    ) -> RefMut<'_, SmolStr, PostboxServiceClient<Hyper>, RandomState> {
        self.0.entry(host.clone()).or_insert_with(|| {
            // TODO: Handle url parsing error
            let host_url: Uri = host.parse().unwrap();

            PostboxServiceClient::new_transport(Hyper::new(host_url).unwrap())
        })
    }
}

#[derive(Clone)]
pub struct SyncServer {
    deps: Arc<Dependencies>,
}

impl SyncServer {
    pub async fn pull_events(&self, clients: &Clients) {
        let hosts = self
            .deps
            .sync_tree
            .scan_prefix(HOST_PREFIX)
            .await
            .flat_map(|res| {
                let key = match res {
                    Ok((key, _)) => key,
                    Err(err) => {
                        let err = ServerError::DbError(err);
                        error!("error occured while getting hosts for sync: {}", err);
                        return None;
                    }
                };
                let (_, host_raw) = key.split_at(HOST_PREFIX.len());
                let host = unsafe { std::str::from_utf8_unchecked(host_raw) };
                Some(SmolStr::new(host))
            });

        for host in hosts {
            if self.is_host_allowed(&host).is_ok() {
                tracing::debug!("pulling from host {host}");
                let mut client = clients.get_client(host.clone());
                if let Ok(queue) = self
                    .generate_request(PullRequest {})
                    .map_err(|_| ())
                    .and_then(|req| client.pull(req).map_err(|_| ()))
                    .and_then(|resp| resp.into_message().map_err(|_| ()))
                    .await
                {
                    for event in queue.event_queue {
                        if let Err(err) = self.push_logic(&host, event).await {
                            error!("error while executing sync event: {}", err);
                        }
                    }
                }
            }
        }
    }

    pub async fn push_events(
        &self,
        clients: &Clients,
        dispatch_rx: &mut UnboundedReceiver<EventDispatch>,
    ) {
        while let Some(EventDispatch { host, event }) = dispatch_rx.recv().await {
            if self.is_host_allowed(&host).is_ok() {
                match self.get_event_queue_raw(&host).await {
                    Ok(raw_queue) => {
                        let maybe_arch_queue = raw_queue
                            .as_ref()
                            .map(|raw_queue| rkyv_arch::<PullResponse>(raw_queue));
                        if !maybe_arch_queue.map_or(false, |v| v.event_queue.is_empty()) {
                            let queue = maybe_arch_queue.map_or_else(PullResponse::default, |v| {
                                v.deserialize(&mut rkyv::Infallible).unwrap()
                            });
                            if let Err(err) = self.push_to_event_queue(&host, queue, event).await {
                                error!("error while pushing to event queue: {}", err);
                            }
                            continue;
                        }

                        let mut client = clients.get_client(host.clone());
                        let mut push_result = self
                            .generate_request(PushRequest {
                                event: Some(event.clone()),
                            })
                            .map_err(|_| ())
                            .and_then(|req| client.push(req).map_err(|_| ()))
                            .await;
                        let mut try_count = 0;
                        while try_count < 5 && push_result.is_err() {
                            push_result = self
                                .generate_request(PushRequest {
                                    event: Some(event.clone()),
                                })
                                .map_err(|_| ())
                                .and_then(|req| client.push(req).map_err(|_| ()))
                                .await;
                            try_count += 1;
                        }

                        if push_result.is_err() {
                            let queue = maybe_arch_queue.map_or_else(PullResponse::default, |v| {
                                v.deserialize(&mut rkyv::Infallible).unwrap()
                            });
                            if let Err(err) = self.push_to_event_queue(&host, queue, event).await {
                                error!("error while pushing to event queue: {}", err);
                            }
                        }
                    }
                    Err(err) => error!("error occured while getting event queue: {}", err),
                }
            }
        }
    }

    pub fn new(deps: Arc<Dependencies>, mut dispatch_rx: UnboundedReceiver<EventDispatch>) -> Self {
        let sync = Self { deps };
        let clients = Clients::new();

        let (initial_pull_tx, initial_pull_rx) = tokio::sync::oneshot::channel();

        tokio::spawn({
            let clients = clients.clone();
            let sync = sync.clone();
            let fut = async move {
                tracing::info!("started task");
                sync.pull_events(&clients).await;
                initial_pull_tx
                    .send(())
                    .expect("failed to send initial pull complete notification");
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    sync.pull_events(&clients).await;
                }
            };
            fut.instrument(tracing::info_span!("federation_pull_task"))
        });

        tokio::spawn({
            let sync = sync.clone();
            let fut = async move {
                tracing::info!("started task");
                initial_pull_rx
                    .await
                    .expect("failed to get initial pull complete notification");
                loop {
                    sync.push_events(&clients, &mut dispatch_rx).await;
                }
            };
            fut.instrument(tracing::info_span!("federation_push_task"))
        });

        sync
    }

    async fn generate_request<Msg: PbMessage>(
        &self,
        msg: Msg,
    ) -> Result<Request<Msg>, ServerError> {
        let data = AuthData {
            server_id: self.deps.config.host.clone(),
            time: get_time_secs(),
        };

        let token = self.keys_manager()?.generate_token(data).await?;
        let token = encode_protobuf_message(&token).freeze();

        let mut req = Request::new(&msg);
        req.get_or_insert_header_map()
            .insert(http::header::AUTHORIZATION, unsafe {
                HeaderValue::from_maybe_shared_unchecked(token)
            });

        Ok(req)
    }

    fn keys_manager(&self) -> Result<&Arc<KeyManager>, ServerError> {
        self.deps
            .key_manager
            .as_ref()
            .ok_or(ServerError::FederationDisabled)
    }

    fn is_host_allowed(&self, host: &str) -> Result<(), ServerError> {
        self.deps
            .config
            .federation
            .as_ref()
            .map_or(Err(ServerError::FederationDisabled), |conf| {
                conf.is_host_allowed(host)
            })
    }

    async fn auth<T>(&self, request: &Request<T>) -> Result<SmolStr, ServerError> {
        let maybe_auth = request
            .header_map()
            .and_then(|h| h.get(http::header::AUTHORIZATION));

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

                let first_verify_result = verify(pubkey);
                // Fetch pubkey if the verification fails, it might have changed
                if let Err(ServerError::CouldntVerifyTokenData) = first_verify_result {
                    keys_manager.invalidate_key(&host);
                    pubkey = get_key().await?;
                } else {
                    return first_verify_result;
                }

                return verify(pubkey);
            }
        }

        Err(ServerError::FailedToAuthSync)
    }

    async fn push_logic(&self, host: &str, event: Event) -> ServerResult<()> {
        if let Some(kind) = event.kind {
            match kind {
                Kind::UserRemovedFromGuild(UserRemovedFromGuild { user_id, guild_id }) => {
                    self.deps
                        .chat_tree
                        .remove_guild_from_guild_list(user_id, guild_id, host)
                        .await?;
                }
                Kind::UserAddedToGuild(UserAddedToGuild { user_id, guild_id }) => {
                    self.deps
                        .chat_tree
                        .add_guild_to_guild_list(user_id, guild_id, host)
                        .await?;
                }
                Kind::UserInvited(_) => todo!(),
                Kind::UserRejectedInvite(_) => todo!(),
            }
        }
        Ok(())
    }

    async fn get_event_queue_raw(&self, host: &str) -> Result<Option<EVec>, ServerError> {
        let key = make_host_key(host);
        let queue = self.deps.sync_tree.get(&key).await?;
        self.deps.sync_tree.remove(&key).await?;
        Ok(queue)
    }

    async fn get_event_queue(&self, host: &str) -> Result<PullResponse, ServerError> {
        self.get_event_queue_raw(host).await.map(|val| {
            val.map_or_else(PullResponse::default, |val| {
                rkyv_arch::<PullResponse>(&val)
                    .deserialize(&mut rkyv::Infallible)
                    .unwrap()
            })
        })
    }

    async fn push_to_event_queue(
        &self,
        host: &str,
        mut queue: PullResponse,
        event: Event,
    ) -> Result<(), ServerError> {
        // TODO: this is a waste, find a way to optimize this
        queue.event_queue.push(event);
        let buf = rkyv_ser(&queue);
        self.deps
            .sync_tree
            .insert(&make_host_key(host), buf.as_ref())
            .await?;
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
