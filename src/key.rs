use std::path::PathBuf;

use crate::api::{
    auth::{auth_service_client::AuthServiceClient, KeyRequest},
    harmonytypes::Token,
};
use ahash::RandomState;
use dashmap::{mapref::one::RefMut, DashMap};
use ed25519_compact::{KeyPair, PublicKey, Seed};
use hrpc::{client::transport::http::Hyper, encode::encode_protobuf_message};
use hyper::Uri;
use prost::Message;
use smol_str::SmolStr;

use crate::ServerError;

pub const KEY_TAG: &str = "ED25519 PUBLIC KEY";

pub fn verify_token(token: &Token, pubkey: &PublicKey) -> Result<(), ServerError> {
    let Token { sig, data } = token;

    let sig = ed25519_compact::Signature::from_slice(sig.as_slice())
        .map_err(|_| ServerError::InvalidTokenSignature)?;

    pubkey
        .verify(data, &sig)
        .map_err(|_| ServerError::CouldntVerifyTokenData)
}

#[derive(Debug)]
pub struct Manager {
    keys: DashMap<SmolStr, PublicKey, RandomState>,
    clients: DashMap<SmolStr, AuthServiceClient<Hyper>, RandomState>,
    federation_key: PathBuf,
}

impl Manager {
    pub fn new(federation_key: PathBuf) -> Self {
        Self {
            federation_key,
            keys: DashMap::default(),
            clients: DashMap::default(),
        }
    }

    pub async fn generate_token(&self, data: impl Message) -> Result<Token, ServerError> {
        let buf = encode_protobuf_message(&data);
        let data = buf.to_vec();

        let key = self.get_own_key().await?;
        let sig = key
            .sk
            .sign(&data, Some(ed25519_compact::Noise::generate()))
            .to_vec();

        Ok(Token { sig, data })
    }

    pub fn invalidate_key(&self, host: &str) {
        self.keys.remove(host);
    }

    pub async fn get_own_key(&self) -> Result<KeyPair, ServerError> {
        match tokio::fs::read(&self.federation_key).await {
            Ok(key) => {
                ed25519_compact::KeyPair::from_slice(&key).map_err(|_| ServerError::CantGetKey)
            }
            Err(err) => {
                if err.kind() == std::io::ErrorKind::NotFound {
                    let new_key = ed25519_compact::KeyPair::from_seed(Seed::generate());
                    tokio::fs::write(&self.federation_key, new_key.as_ref())
                        .await
                        .map(|_| new_key)
                        .map_err(|_| ServerError::CantGetKey)
                } else {
                    Err(ServerError::CantGetKey)
                }
            }
        }
    }

    pub async fn get_key(&self, host: SmolStr) -> Result<PublicKey, ServerError> {
        let key = if let Some(key) = self.keys.get(&host) {
            *key
        } else {
            let key = self
                .get_client(host.clone())
                .value_mut()
                .key(KeyRequest {})
                .await
                .map_err(|_| ServerError::CantGetHostKey(host.clone()))?
                .into_message()
                .await?
                .key;
            let key = ed25519_compact::PublicKey::from_slice(key.as_slice())
                .map_err(|_| ServerError::CantGetHostKey(host.clone()))?;
            self.keys.insert(host, key);
            key
        };

        Ok(key)
    }

    fn get_client(
        &self,
        host: SmolStr,
    ) -> RefMut<'_, SmolStr, AuthServiceClient<Hyper>, RandomState> {
        self.clients.entry(host.clone()).or_insert_with(|| {
            let host_url: Uri = host.parse().unwrap();

            AuthServiceClient::new_transport(Hyper::new(host_url).unwrap())
        })
    }
}
