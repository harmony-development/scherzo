#![allow(dead_code)]

use super::{chat::ChatTree, *};

use ahash::RandomState;
use dashmap::{mapref::one::RefMut, DashMap};
use ed25519_compact::PublicKey;
use harmony_rust_sdk::api::{
    auth::auth_service_client::AuthServiceClient,
    exports::{
        hrpc::{async_trait, server::Socket, Request},
        prost::Message,
    },
    harmonytypes::Token,
    sync::{event::*, postbox_service_client::PostboxServiceClient, *},
};

use reqwest::Url;

type Clients = (PostboxServiceClient, AuthServiceClient);

fn parse_pem(key: String) -> Result<ed25519_compact::PublicKey, ServerError> {
    let pem = pem::parse(key).map_err(|_| ServerError::FailedToAuthSync)?;

    if pem.tag != "ED25519 PUBLIC KEY" {
        return Err(ServerError::FailedToAuthSync);
    }

    ed25519_compact::PublicKey::from_slice(pem.contents.as_slice())
        .map_err(|_| ServerError::FailedToAuthSync)
}

pub struct SyncServer {
    keys: DashMap<String, PublicKey, RandomState>,
    clients: DashMap<String, Clients, RandomState>,
    chat_tree: ChatTree,
}

impl SyncServer {
    pub fn new(chat_tree: ChatTree) -> Self {
        Self {
            clients: DashMap::default(),
            keys: DashMap::default(),
            chat_tree,
        }
    }

    fn invalidate_key(&self, host: &str) {
        self.keys.remove(host);
    }

    async fn get_key(&self, host: &str) -> Result<PublicKey, ServerError> {
        let key = if let Some(key) = self.keys.get(host) {
            *key
        } else {
            let key = self
                .get_client(host)
                .1
                .key(())
                .await
                .map_err(|_| ServerError::FailedToAuthSync)?
                .key;
            let key = parse_pem(key).map_err(|_| ServerError::FailedToAuthSync)?;
            self.keys.insert(host.to_string(), key);
            key
        };

        Ok(key)
    }

    fn get_client<'a>(&'a self, host: &str) -> RefMut<'a, String, Clients, RandomState> {
        if let Some(client) = self.clients.get_mut(host) {
            client
        } else {
            let http = reqwest::Client::new(); // each server gets its own http client
            let host_url: Url = host.parse().unwrap();

            let sync_client = PostboxServiceClient::new(http.clone(), host_url.clone()).unwrap();
            let auth_client = AuthServiceClient::new(http, host_url).unwrap();

            self.clients
                .insert(host.to_string(), (sync_client, auth_client));
            self.clients.get_mut(host).unwrap()
        }
    }

    // TODO: improve errors
    async fn auth<T>(&self, request: &Request<T>) -> Result<String, ServerError> {
        let maybe_auth = request.get_header(&http::header::AUTHORIZATION);

        if let Some(auth) = maybe_auth.map(|h| h.as_bytes()) {
            let Token { sig, data } =
                Token::decode(auth).map_err(|_| ServerError::FailedToAuthSync)?;

            let sig = ed25519_compact::Signature::from_slice(sig.as_slice())
                .map_err(|_| ServerError::FailedToAuthSync)?;

            let AuthData { host, time } =
                AuthData::decode(data.as_slice()).map_err(|_| ServerError::FailedToAuthSync)?;

            let cur_time = get_time_secs();
            // Check time variance (1 minute)
            if time < cur_time + 30 && time > cur_time - 30 {
                let pubkey = self.get_key(&host).await?;

                return pubkey
                    .verify(&data, &sig)
                    .map(|_| host)
                    .map_err(|_| ServerError::FailedToAuthSync);
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

    async fn pull(&self, host: String, socket: Socket<Ack, Syn>) {
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
