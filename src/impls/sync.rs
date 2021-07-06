#![allow(dead_code)]

use super::*;

use ahash::RandomState;
use dashmap::{mapref::one::Ref, DashMap};
use harmony_rust_sdk::api::{
    auth::auth_service_client::AuthServiceClient,
    exports::hrpc::{async_trait, server::Socket, Request},
    sync::{postbox_service_client::PostboxServiceClient, *},
};

use reqwest::Url;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
struct AuthToken {
    #[serde(rename = "self")]
    host: String,
    time: u64,
}

pub struct SyncServer {
    keys: DashMap<String, String, RandomState>,
    clients: DashMap<String, (PostboxServiceClient, AuthServiceClient), RandomState>,
}

impl SyncServer {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            clients: DashMap::default(),
            keys: DashMap::default(),
        }
    }

    fn get_client<'a>(
        &'a self,
        host: &str,
    ) -> Ref<'a, String, (PostboxServiceClient, AuthServiceClient), RandomState> {
        if let Some(client) = self.clients.get(host) {
            client
        } else {
            let http = reqwest::Client::new(); // each server gets its own http client
            let host_url: Url = host.parse().unwrap();

            let sync_client = PostboxServiceClient::new(http.clone(), host_url.clone()).unwrap();
            let auth_client = AuthServiceClient::new(http, host_url).unwrap();

            self.clients
                .insert(host.to_string(), (sync_client, auth_client));
            self.clients.get(host).unwrap()
        }
    }

    async fn auth<T>(&self, request: Request<T>) -> Result<AuthToken, ServerError> {
        let maybe_auth = request.get_header(&http::header::AUTHORIZATION);
        if let Some(auth) = maybe_auth.map(|h| h.as_bytes()) {
            todo!()
        }
        todo!()
    }
}

#[async_trait]
impl postbox_service_server::PostboxService for SyncServer {
    type Error = ServerError;

    type PullValidationType = ();

    async fn pull_validation(
        &self,
        request: Request<Option<Ack>>,
    ) -> Result<Self::PullValidationType, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn pull(&self, validation_value: Self::PullValidationType, socket: Socket<Ack, Syn>) {
        todo!()
    }

    async fn push(&self, request: Request<Event>) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }
}
