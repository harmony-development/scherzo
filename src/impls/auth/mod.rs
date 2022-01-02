use std::time::Duration;

use ahash::RandomState;
use dashmap::DashMap;
use harmony_rust_sdk::api::{
    auth::{next_step_request::form_fields::Field, *},
    profile::{Profile, UserStatus},
};
use hrpc::server::gen_prelude::BoxFuture;
use hyper::{http, HeaderMap};
use rand::{Rng, SeedableRng};
use tokio::sync::mpsc::{self, Sender};
use tracing::Instrument;

use crate::key::{self as keys, Manager as KeyManager};

use super::{gen_rand_arr, gen_rand_inline_str, gen_rand_u64, get_time_secs, prelude::*};

use db::{
    auth::*,
    profile::{
        make_foreign_to_local_user_key, make_local_to_foreign_user_key, make_user_profile_key,
    },
};

pub mod begin_auth;
pub mod check_logged_in;
pub mod delete_user;
pub mod federate;
pub mod key;
pub mod login_federated;
pub mod next_step;
pub mod step_back;
pub mod stream_steps;

const SESSION_EXPIRE: u64 = 60 * 60 * 24 * 2;

pub fn get_token_from_header_map(headers: &HeaderMap) -> &str {
    headers
        .get(http::header::AUTHORIZATION)
        .map_or_else(
            || {
                // Specific handling for web clients
                headers
                    .get(http::header::SEC_WEBSOCKET_PROTOCOL)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|v| v.split(',').map(str::trim).last())
            },
            |val| val.to_str().ok(),
        )
        .unwrap_or("")
}

pub trait AuthExt {
    fn auth_with<'a>(&'a self, auth_id: &'a str) -> BoxFuture<'a, Result<u64, ServerError>>;
    fn auth<'a, T>(&'a self, request: &'a Request<T>) -> BoxFuture<'a, Result<u64, ServerError>> {
        let auth_id = request
            .header_map()
            .map_or(Err(ServerError::Unauthenticated), |headers| {
                Ok(get_token_from_header_map(headers))
            });

        match auth_id {
            Ok(auth_id) => self.auth_with(auth_id),
            Err(err) => Box::pin(std::future::ready(Err(err))),
        }
    }
}

impl AuthExt for Dependencies {
    fn auth_with<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<u64, ServerError>> {
        Box::pin(async move {
            self.auth_tree
                .get(auth_key(token))
                .await?
                .map_or(Err(ServerError::Unauthenticated), |raw| Ok(deser_id(raw)))
        })
    }
}
#[derive(Clone)]
pub struct AuthServer {
    step_map: Arc<DashMap<SmolStr, Vec<AuthStep>, RandomState>>,
    send_step: Arc<DashMap<SmolStr, Sender<AuthStep>, RandomState>>,
    queued_steps: Arc<DashMap<SmolStr, Vec<AuthStep>, RandomState>>,
    disable_ratelimits: bool,
    deps: Arc<Dependencies>,
}

impl AuthServer {
    pub fn new(deps: Arc<Dependencies>) -> Self {
        let att = deps.auth_tree.clone();
        let ptt = deps.profile_tree.clone();

        tokio::spawn(
            (async move {
                tracing::info!("starting auth session expiration check thread");

                async fn scan_tree_for(
                    att: &Tree,
                    prefix: &[u8],
                ) -> ServerResult<Vec<(u64, EVec)>> {
                    let len = prefix.len();
                    att.scan_prefix(prefix)
                        .await
                        .try_fold(Vec::new(), move |mut all, res| {
                            let (key, val) = res.map_err(ServerError::from)?;
                            all.push((
                                // Safety: the right portion of the key after split at the prefix length MUST be a valid u64
                                deser_id(key.split_at(len).1),
                                val,
                            ));
                            ServerResult::Ok(all)
                        })
                }

                loop {
                    // Safety: we never insert non u64 keys for tokens [tag:token_u64_key]
                    let tokens = scan_tree_for(&att.inner, TOKEN_PREFIX).await;
                    // Safety: we never insert non u64 keys for atimes [tag:atime_u64_key]
                    let atimes = scan_tree_for(&att.inner, ATIME_PREFIX).await;

                    match tokens.and_then(|tokens| Ok((tokens, atimes?))) {
                        Ok((tokens, atimes)) => {
                            let mut batch = Batch::default();
                            for (id, raw_token) in tokens {
                                if let Ok(profile) = ptt.get_profile_logic(id).await {
                                    for (oid, raw_atime) in &atimes {
                                        if id.eq(oid) {
                                            // Safety: raw_atime's we store are always u64s [tag:atime_u64_value]
                                            let secs = deser_id(raw_atime);
                                            let auth_how_old = get_time_secs() - secs;
                                            // Safety: all of our tokens are valid str's, we never generate invalid ones [ref:alphanumeric_auth_token_gen]
                                            let token = unsafe {
                                                std::str::from_utf8_unchecked(raw_token.as_ref())
                                            };
                                            let auth_key = auth_key(token);

                                            let Ok(token_exists) = att.contains_key(&auth_key).await else { continue; };
                                            if token_exists {
                                                // [ref:atime_u64_key] [ref:atime_u64_value]
                                                batch.insert(
                                                    atime_key(id),
                                                    get_time_secs().to_be_bytes(),
                                                );
                                            } else if !profile.is_bot
                                                && auth_how_old >= SESSION_EXPIRE
                                            {
                                                tracing::debug!("user {} session has expired", id);
                                                batch.remove(auth_key);
                                                batch.remove(token_key(id));
                                                batch.remove(atime_key(id));
                                            }
                                        }
                                    }
                                }
                            }
                            if let Err(err) = att
                                .inner
                                .apply_batch(batch)
                                .await
                                .map_err(ServerError::DbError)
                            {
                                tracing::error!("error applying auth token batch: {}", err);
                            }
                        }
                        Err(err) => {
                            tracing::error!("error scanning tree for tokens: {}", err);
                        }
                    }

                    std::thread::sleep(Duration::from_secs(60 * 5));
                }
            })
            .instrument(tracing::info_span!("auth_session_check")),
        );

        Self {
            step_map: DashMap::default().into(),
            send_step: DashMap::default().into(),
            queued_steps: DashMap::default().into(),
            disable_ratelimits: deps.config.policy.ratelimit.disable,
            deps,
        }
    }

    pub fn batch(mut self) -> Self {
        self.disable_ratelimits = true;
        self
    }

    async fn is_token_valid(&self, token: &str) -> Result<bool, ServerError> {
        self.deps.auth_with(token).await.map_or_else(
            |err| {
                matches!(err, ServerError::Unauthenticated)
                    .then(|| Ok(false))
                    .unwrap_or(Err(err))
            },
            |_| Ok(true),
        )
    }

    // [tag:alphanumeric_auth_token_gen] [tag:auth_token_length]
    async fn gen_auth_token(&self) -> Result<SmolStr, ServerError> {
        let mut rng = rand::rngs::SmallRng::from_entropy();
        let mut raw = gen_rand_arr::<_, 22>(&mut rng);
        let mut token = unsafe { std::str::from_utf8_unchecked(&raw) };

        while self.is_token_valid(token).await? {
            raw = gen_rand_arr::<_, 22>(&mut rng);
            token = unsafe { std::str::from_utf8_unchecked(&raw) };
        }

        Ok(SmolStr::new_inline(token))
    }

    async fn gen_user_id(&self) -> Result<u64, ServerError> {
        let mut rng = rand::rngs::SmallRng::from_entropy();
        let mut id: u64 = rng.gen_range(1..=std::u64::MAX);
        while self.deps.auth_tree.contains_key(&id.to_be_bytes()).await? {
            id = rng.gen_range(1..=std::u64::MAX);
        }
        Ok(id)
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
}

impl auth_service_server::AuthService for AuthServer {
    impl_unary_handlers! {
        #[rate(3, 5)]
        check_logged_in, CheckLoggedInRequest, CheckLoggedInResponse;
        #[rate(3, 1)]
        federate, FederateRequest, FederateResponse;
        #[rate(1, 5)]
        login_federated, LoginFederatedRequest, LoginFederatedResponse;
        #[rate(1, 5)]
        key, KeyRequest, KeyResponse;
        #[rate(2, 5)]
        begin_auth, BeginAuthRequest, BeginAuthResponse;
        #[rate(5, 5)]
        next_step, NextStepRequest, NextStepResponse;
        #[rate(5, 5)]
        step_back, StepBackRequest, StepBackResponse;
    }

    impl_ws_handlers! {
        #[rate(1, 5)]
        stream_steps, StreamStepsRequest, StreamStepsResponse;
    }
}

#[derive(Clone)]
pub struct AuthTree {
    pub inner: Tree,
}

impl AuthTree {
    impl_db_methods!(inner);

    pub async fn new(db: &Db) -> DbResult<Self> {
        Ok(Self {
            inner: db.open_tree(b"auth").await?,
        })
    }

    pub async fn generate_single_use_token(&self, value: impl Into<EVec>) -> ServerResult<SmolStr> {
        fn generate() -> (SmolStr, Vec<u8>) {
            let token = gen_rand_inline_str();
            let key = {
                let hashed = hash_token(token.as_bytes());
                single_use_token_key(hashed.as_ref())
            };
            (token, key)
        }

        let (token, key) = {
            let (mut token, mut key) = generate();
            while self.contains_key(&key).await? {
                (token, key) = generate();
            }
            (token, key)
        };

        self.insert(key, value.into())
            .await
            .map_err(ServerError::from)?;

        Ok(token)
    }

    pub async fn validate_single_use_token(&self, token: Vec<u8>) -> ServerResult<EVec> {
        if token.is_empty() {
            bail!((
                "h.invalid-single-use-token",
                "single use token can't be empty"
            ));
        }

        let token_hashed = hash_token(token);
        let val = self
            .remove(&single_use_token_key(token_hashed.as_ref()))
            .await?
            .ok_or(ServerError::InvalidRegistrationToken)?;

        Ok(val)
    }

    pub async fn get_user_id(&self, email: &str) -> ServerResult<u64> {
        let maybe_user_id = self.get(email.as_bytes()).await?.map(deser_id);

        maybe_user_id
            .ok_or_else(|| ServerError::WrongEmailOrPassword {
                email: email.into(),
            })
            .map_err(Into::into)
    }
}

pub fn initial_auth_step() -> AuthStep {
    AuthStep {
        can_go_back: false,
        fallback_url: String::default(),
        step: Some(auth_step::Step::Choice(auth_step::Choice {
            title: "initial".to_string(),
            options: ["login", "register", "other-options"]
                .iter()
                .map(ToString::to_string)
                .collect(),
        })),
    }
}

pub fn back_to_inital_step() -> AuthStep {
    AuthStep {
        can_go_back: false,
        fallback_url: String::default(),
        step: Some(auth_step::Step::Choice(auth_step::Choice {
            title: "success".to_string(),
            options: ["back-to-initial"]
                .iter()
                .map(ToString::to_string)
                .collect(),
        })),
    }
}

// this uses a constant salt because we want the output to be deterministic
fn hash_token(token: impl AsRef<[u8]>) -> String {
    use argon2::{
        password_hash::{PasswordHasher, Salt, SaltString},
        Argon2,
    };

    let salt = SaltString::b64_encode(&[0; Salt::RECOMMENDED_LENGTH]).expect("cant fail");
    let argon2 = Argon2::default();
    argon2
        .hash_password(token.as_ref(), &salt)
        .expect("todo handle failure")
        .to_string()
}

fn hash_password(pass: impl AsRef<[u8]>) -> String {
    use argon2::{
        password_hash::{PasswordHasher, SaltString},
        Argon2,
    };

    let salt = SaltString::generate(&mut rand::thread_rng());
    let argon2 = Argon2::default();
    argon2
        .hash_password(pass.as_ref(), &salt)
        .expect("todo handle failure")
        .to_string()
}

fn verify_password(pass: impl AsRef<[u8]>, hash: &str) -> bool {
    use argon2::{
        password_hash::{PasswordHash, PasswordVerifier},
        Argon2,
    };

    let Ok(pass_hash) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(pass.as_ref(), &pass_hash)
        .is_ok()
}

const PASSWORD_FIELD_ERR: ServerError = ServerError::WrongTypeForField {
    name: SmolStr::new_inline("password"),
    expected: SmolStr::new_inline("bytes"),
};

const EMAIL_FIELD_ERR: ServerError = ServerError::WrongTypeForField {
    name: SmolStr::new_inline("email"),
    expected: SmolStr::new_inline("email"),
};

const USERNAME_FIELD_ERR: ServerError = ServerError::WrongTypeForField {
    name: SmolStr::new_inline("username"),
    expected: SmolStr::new_inline("text"),
};

const TOKEN_FIELD_ERR: ServerError = ServerError::WrongTypeForField {
    name: SmolStr::new_inline("token"),
    expected: SmolStr::new_inline("bytes"),
};

#[inline(always)]
fn try_get_string(values: &mut Vec<Field>, err: ServerError) -> ServerResult<String> {
    if let Some(Field::String(value)) = values.pop() {
        Ok(value)
    } else {
        Err(err.into())
    }
}

#[inline(always)]
fn try_get_bytes(values: &mut Vec<Field>, err: ServerError) -> ServerResult<Vec<u8>> {
    if let Some(Field::Bytes(value)) = values.pop() {
        Ok(value)
    } else {
        Err(err.into())
    }
}

#[inline(always)]
fn try_get_email(values: &mut Vec<Field>) -> ServerResult<String> {
    try_get_string(values, EMAIL_FIELD_ERR)
}

#[inline(always)]
fn try_get_username(values: &mut Vec<Field>) -> ServerResult<String> {
    try_get_string(values, USERNAME_FIELD_ERR)
}

#[inline(always)]
fn try_get_password(values: &mut Vec<Field>) -> ServerResult<Vec<u8>> {
    try_get_bytes(values, PASSWORD_FIELD_ERR)
}

#[inline(always)]
fn try_get_token(values: &mut Vec<Field>) -> ServerResult<Vec<u8>> {
    try_get_bytes(values, TOKEN_FIELD_ERR)
}
