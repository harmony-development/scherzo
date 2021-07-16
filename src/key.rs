use std::path::PathBuf;

use ahash::RandomState;
use dashmap::{mapref::one::RefMut, DashMap};
use ed25519_compact::{KeyPair, PublicKey, Seed};
use harmony_rust_sdk::api::{auth::auth_service_client::AuthServiceClient, harmonytypes::Token};
use reqwest::Url;
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

fn parse_pem(key: String, host: SmolStr) -> Result<ed25519_compact::PublicKey, ServerError> {
    let pem = pem::parse(key).map_err(|_| ServerError::CantGetHostKey(host.clone()))?;

    if pem.tag != KEY_TAG {
        return Err(ServerError::CantGetHostKey(host));
    }

    ed25519_compact::PublicKey::from_slice(pem.contents.as_slice())
        .map_err(|_| ServerError::CantGetHostKey(host))
}

#[derive(Debug)]
pub struct Manager {
    keys: DashMap<SmolStr, PublicKey, RandomState>,
    clients: DashMap<SmolStr, AuthServiceClient, RandomState>,
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
                .key(())
                .await
                .map_err(|_| ServerError::CantGetHostKey(host.clone()))?
                .key;
            let key = parse_pem(key, host.clone())?;
            self.keys.insert(host, key);
            key
        };

        Ok(key)
    }

    fn get_client(&self, host: SmolStr) -> RefMut<'_, SmolStr, AuthServiceClient, RandomState> {
        self.clients.entry(host.clone()).or_insert_with(|| {
            let http = reqwest::Client::new(); // each server gets its own http client
            let host_url: Url = host.parse().unwrap();

            AuthServiceClient::new(http, host_url).unwrap()
        })
    }
}
