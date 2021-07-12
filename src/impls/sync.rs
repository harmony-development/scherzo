#![allow(dead_code)]

use super::{chat::ChatTree, keys_manager::KeysManager, *};

use ahash::RandomState;
use dashmap::{mapref::one::RefMut, DashMap};
use harmony_rust_sdk::api::{
    exports::{
        hrpc::{async_trait, server::Socket, Request},
        prost::Message,
    },
    harmonytypes::Token,
    sync::{event::*, postbox_service_client::PostboxServiceClient, *},
};
use reqwest::Url;
use tokio::sync::mpsc::UnboundedReceiver;

use triomphe::Arc;

pub struct EventDispatch {
    pub host: String,
    pub event: Event,
}

#[derive(Clone)]
pub struct SyncServer {
    chat_tree: ChatTree,
    keys_manager: Arc<KeysManager>,
    clients: DashMap<String, PostboxServiceClient, RandomState>,
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
            clients: DashMap::default(),
        };
        let sync2 = sync.clone();
        tokio::spawn(async move {
            loop {
                while let Some(dispatch) = dispatch_rx.recv().await {
                    // TODO: push to queue if fails
                    sync2
                        .get_client(&dispatch.host)
                        .push(dispatch.event)
                        .await
                        .unwrap();
                }
            }
        });
        sync
    }

    fn get_client<'a>(
        &'a self,
        host: &str,
    ) -> RefMut<'a, String, PostboxServiceClient, RandomState> {
        if let Some(client) = self.clients.get_mut(host) {
            client
        } else {
            let http = reqwest::Client::new(); // each server gets its own http client
            let host_url: Url = host.parse().unwrap();

            let auth_client = PostboxServiceClient::new(http, host_url).unwrap();

            self.clients.insert(host.to_string(), auth_client);
            self.clients.get_mut(host).unwrap()
        }
    }

    // TODO: improve errors
    async fn auth<T>(&self, request: &Request<T>) -> Result<String, ServerError> {
        let maybe_auth = request.get_header(&http::header::AUTHORIZATION);

        if let Some(auth) = maybe_auth.map(|h| h.as_bytes()) {
            let token = Token::decode(auth).map_err(|_| ServerError::InvalidToken)?;

            let AuthData { host, time } = AuthData::decode(token.data.as_slice())
                .map_err(|_| ServerError::InvalidTokenData)?;

            let cur_time = get_time_secs();
            // Check time variance (1 minute)
            if time < cur_time + 30 && time > cur_time - 30 {
                let pubkey = self.keys_manager.get_key(&host).await?;

                return verify_token(&token, &pubkey).map(|_| host);
            }
        }

        Err(ServerError::FailedToAuthSync)
    }
}

#[async_trait]
impl postbox_service_server::PostboxService for SyncServer {
    type Error = ServerError;

    type PullValidationType = String;

    async fn pull_validation(&self, request: Request<Option<Ack>>) -> Result<String, Self::Error> {
        self.auth(&request).await
    }

    async fn pull(&self, _host: String, _socket: Socket<Ack, Syn>) {
        todo!()
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
