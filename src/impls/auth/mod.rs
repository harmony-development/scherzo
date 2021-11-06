use std::time::Duration;

use ahash::RandomState;
use dashmap::DashMap;
use harmony_rust_sdk::api::{
    auth::{next_step_request::form_fields::Field, *},
    profile::{Profile, UserStatus},
};
use hyper::HeaderMap;
use sha3::Digest;
use tokio::sync::mpsc::{self, Sender};
use tower::limit::RateLimitLayer;

use crate::{
    key::{self as keys, Manager as KeyManager},
    set_proto_name_layer,
};

use super::{gen_rand_arr, gen_rand_inline_str, gen_rand_u64, get_time_secs, prelude::*};

use db::{
    auth::*,
    profile::{
        make_foreign_to_local_user_key, make_local_to_foreign_user_key, make_user_profile_key,
    },
};

pub mod begin_auth;
pub mod check_logged_in;
pub mod federate;
pub mod key;
pub mod login_federated;
pub mod next_step;
pub mod step_back;
pub mod stream_steps;

const SESSION_EXPIRE: u64 = 60 * 60 * 24 * 2;

pub type SessionMap = Arc<DashMap<SmolStr, u64, RandomState>>;

pub trait AuthExt {
    fn auth_header_map(&self, headers: &HeaderMap) -> ServerResult<u64>;
    fn auth<T>(&self, request: &Request<T>) -> ServerResult<u64> {
        let headers = request.header_map();
        self.auth_header_map(headers)
    }
}

impl AuthExt for DashMap<SmolStr, u64, RandomState> {
    fn auth_header_map(&self, headers: &HeaderMap) -> ServerResult<u64> {
        let auth_id = headers
            .get(&http::header::AUTHORIZATION)
            .map_or_else(
                || {
                    // Specific handling for web clients
                    headers
                        .get(&http::header::SEC_WEBSOCKET_PROTOCOL)
                        .and_then(|val| {
                            val.to_str()
                                .ok()
                                .and_then(|v| v.split(',').nth(1).map(str::trim))
                        })
                },
                |val| val.to_str().ok(),
            )
            .unwrap_or("");

        self.get(auth_id)
            .as_deref()
            .copied()
            .map_or(Err(ServerError::Unauthenticated.into()), Ok)
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
        let vs = deps.valid_sessions.clone();

        std::thread::spawn(move || {
            let _guard = tracing::info_span!("auth_session_check").entered();
            tracing::info!("starting auth session expiration check thread");

            // Safety: the right portion of the key after split at the prefix length MUST be a valid u64
            unsafe fn scan_tree_for(
                att: &dyn Tree,
                prefix: &[u8],
            ) -> ServerResult<Vec<(u64, Vec<u8>)>> {
                let len = prefix.len();
                att.scan_prefix(prefix)
                    .try_fold(Vec::new(), move |mut all, res| {
                        let (key, val) = res.map_err(ServerError::from)?;
                        all.push((
                            u64::from_be_bytes(key.split_at(len).1.try_into().unwrap_unchecked()),
                            val,
                        ));
                        ServerResult::Ok(all)
                    })
            }

            loop {
                // Safety: we never insert non u64 keys for tokens [tag:token_u64_key]
                let tokens = unsafe { scan_tree_for(att.inner.as_ref(), TOKEN_PREFIX) };
                // Safety: we never insert non u64 keys for atimes [tag:atime_u64_key]
                let atimes = unsafe { scan_tree_for(att.inner.as_ref(), ATIME_PREFIX) };

                match tokens.and_then(|tokens| Ok((tokens, atimes?))) {
                    Ok((tokens, atimes)) => {
                        let mut batch = Batch::default();
                        for (id, raw_token) in tokens {
                            if let Ok(profile) = ptt.get_profile_logic(id) {
                                for (oid, raw_atime) in &atimes {
                                    if id.eq(oid) {
                                        // Safety: raw_atime's we store are always u64s [tag:atime_u64_value]
                                        let secs = u64::from_be_bytes(unsafe {
                                            raw_atime.as_slice().try_into().unwrap_unchecked()
                                        });
                                        let auth_how_old = get_time_secs() - secs;
                                        // Safety: all of our tokens are valid str's, we never generate invalid ones [ref:alphanumeric_auth_token_gen]
                                        let token = unsafe {
                                            std::str::from_utf8_unchecked(raw_token.as_ref())
                                        };

                                        if vs.contains_key(token) {
                                            // [ref:atime_u64_key] [ref:atime_u64_value]
                                            batch.insert(
                                                atime_key(id).to_vec(),
                                                get_time_secs().to_be_bytes().to_vec(),
                                            );
                                        } else if !profile.is_bot && auth_how_old >= SESSION_EXPIRE
                                        {
                                            tracing::debug!("user {} session has expired", id);
                                            batch.remove(token_key(id).to_vec());
                                            batch.remove(atime_key(id).to_vec());
                                            vs.remove(token);
                                        } else {
                                            // Safety: all of our tokens are 22 chars long, so this can never panic [ref:auth_token_length]
                                            vs.insert(SmolStr::new_inline(token), id);
                                        }
                                    }
                                }
                            }
                        }
                        if let Err(err) = att.inner.apply_batch(batch).map_err(ServerError::DbError)
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
        });

        Self {
            step_map: DashMap::default().into(),
            send_step: DashMap::default().into(),
            queued_steps: DashMap::default().into(),
            disable_ratelimits: deps.config.policy.disable_ratelimits,
            deps,
        }
    }

    pub fn batch(mut self) -> Self {
        self.disable_ratelimits = true;
        self
    }

    // [tag:alphanumeric_auth_token_gen] [tag:auth_token_length]
    fn gen_auth_token(&self) -> SmolStr {
        let mut rng = rand::thread_rng();
        let mut raw = gen_rand_arr::<_, 22>(&mut rng);
        let mut token = unsafe { std::str::from_utf8_unchecked(&raw) };
        while self.deps.valid_sessions.contains_key(token) {
            raw = gen_rand_arr::<_, 22>(&mut rng);
            token = unsafe { std::str::from_utf8_unchecked(&raw) };
        }
        SmolStr::new_inline(token)
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
        stream_steps, StreamStepsRequest, StreamStepsResponse;
    }

    fn stream_steps_middleware(&self, _endpoint: &'static str) -> Option<HrpcLayer> {
        let rate = self
            .disable_ratelimits
            .then(|| RateLimitLayer::new(1, Duration::from_secs(5)));

        rate.map(|r| {
            HrpcLayer::new(
                tower::ServiceBuilder::new()
                    .layer(set_proto_name_layer())
                    .layer(r)
                    .into_inner(),
            )
        })
        .or_else(|| Some(HrpcLayer::new(set_proto_name_layer())))
    }
}

#[derive(Clone)]
pub struct AuthTree {
    pub inner: ArcTree,
}

impl AuthTree {
    impl_db_methods!(inner);

    pub fn new(db: &dyn Db) -> DbResult<Self> {
        Ok(Self {
            inner: db.open_tree(b"auth")?,
        })
    }
    pub fn put_rand_reg_token(&self) -> ServerResult<SmolStr> {
        // TODO: check if the token is already in tree
        let token = gen_rand_inline_str();
        {
            let hashed = hash_password(token.as_bytes());
            let key = reg_token_key(hashed.as_ref());
            self.inner.insert(&key, &[]).map_err(ServerError::from)?;
        }
        Ok(token)
    }
}

#[inline(always)]
fn hash_password(raw: impl AsRef<[u8]>) -> impl AsRef<[u8]> {
    sha3::Sha3_512::digest(raw.as_ref())
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
