use std::{convert::TryInto, time::Duration};

use ahash::RandomState;
use dashmap::DashMap;
use harmony_rust_sdk::api::{
    auth::{next_step_request::form_fields::Field, *},
    exports::{
        hrpc::{
            encode_protobuf_message, server::ServerError as HrpcServerError, server::WriteSocket,
            warp::reply::Response, Request,
        },
        prost::Message,
    },
    profile::{Profile, UserStatus},
};
use scherzo_derive::*;
use sha3::Digest;
use smol_str::SmolStr;
use tokio::sync::mpsc::{self, Sender};
use triomphe::Arc;

use crate::{
    config::FederationConfig,
    key::{self, Manager as KeyManager},
};

use crate::{
    db::{
        auth::*,
        profile::{
            make_foreign_to_local_user_key, make_local_to_foreign_user_key, make_user_profile_key,
        },
        ArcTree, Batch, Tree,
    },
    http,
    impls::{gen_rand_inline_str, gen_rand_u64, get_time_secs},
    set_proto_name, ServerError, ServerResult,
};

use super::{gen_rand_arr, profile::ProfileTree, Dependencies};

const SESSION_EXPIRE: u64 = 60 * 60 * 24 * 2;

pub type SessionMap = Arc<DashMap<SmolStr, u64, RandomState>>;

pub fn check_auth<T>(
    valid_sessions: &SessionMap,
    request: &Request<T>,
) -> Result<u64, ServerError> {
    let auth_id = request
        .get_header(&http::header::AUTHORIZATION)
        .map_or_else(
            || {
                // Specific handling for web clients
                request
                    .get_header(&http::header::SEC_WEBSOCKET_PROTOCOL)
                    .and_then(|val| {
                        val.to_str()
                            .ok()
                            .and_then(|v| v.split(',').nth(1).map(str::trim))
                    })
            },
            |val| val.to_str().ok(),
        )
        .unwrap_or("");

    valid_sessions
        .get(auth_id)
        .as_deref()
        .copied()
        .map_or(Err(ServerError::Unauthenticated), Ok)
}

pub struct AuthServer {
    valid_sessions: SessionMap,
    step_map: DashMap<SmolStr, Vec<AuthStep>, RandomState>,
    send_step: DashMap<SmolStr, Sender<AuthStep>, RandomState>,
    auth_tree: ArcTree,
    profile_tree: ProfileTree,
    keys_manager: Option<Arc<KeyManager>>,
    federation_config: Option<FederationConfig>,
    disable_ratelimits: bool,
}

impl AuthServer {
    pub fn new(deps: &Dependencies) -> Self {
        let att = deps.auth_tree.clone();
        let ptt = deps.profile_tree.clone();
        let vs = deps.valid_sessions.clone();

        std::thread::spawn(move || {
            let _guard = tracing::info_span!("auth_session_check").entered();
            tracing::info!("starting auth session expiration check thread");

            // Safety: the right portion of the key after split at the prefix length MUST be a valid u64
            unsafe fn scan_tree_for(att: &dyn Tree, prefix: &[u8]) -> Vec<(u64, Vec<u8>)> {
                let len = prefix.len();
                att.scan_prefix(prefix)
                    .map(move |res| {
                        let (key, val) = res.unwrap();
                        (
                            u64::from_be_bytes(key.split_at(len).1.try_into().unwrap_unchecked()),
                            val,
                        )
                    })
                    .collect()
            }

            loop {
                // Safety: we never insert non u64 keys for tokens [tag:token_u64_key]
                let tokens = unsafe { scan_tree_for(att.as_ref(), TOKEN_PREFIX) };
                // Safety: we never insert non u64 keys for atimes [tag:atime_u64_key]
                let atimes = unsafe { scan_tree_for(att.as_ref(), ATIME_PREFIX) };

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
                                let token =
                                    unsafe { std::str::from_utf8_unchecked(raw_token.as_ref()) };

                                if vs.contains_key(token) {
                                    // [ref:atime_u64_key] [ref:atime_u64_value]
                                    batch.insert(&atime_key(id), &get_time_secs().to_be_bytes());
                                } else if !profile.is_bot && auth_how_old >= SESSION_EXPIRE {
                                    tracing::debug!("user {} session has expired", id);
                                    batch.remove(&token_key(id));
                                    batch.remove(&atime_key(id));
                                    vs.remove(token);
                                } else {
                                    // Safety: all of our tokens are 22 chars long, so this can never panic [ref:auth_token_length]
                                    vs.insert(SmolStr::new_inline(token), id);
                                }
                            }
                        }
                    }
                }
                att.apply_batch(batch).unwrap();
                std::thread::sleep(Duration::from_secs(60 * 5));
            }
        });

        Self {
            valid_sessions: deps.valid_sessions.clone(),
            step_map: DashMap::default(),
            send_step: DashMap::default(),
            auth_tree: deps.auth_tree.clone(),
            profile_tree: deps.profile_tree.clone(),
            keys_manager: deps.key_manager.clone(),
            federation_config: deps.config.federation.clone(),
            disable_ratelimits: deps.config.disable_ratelimits,
        }
    }

    // [tag:alphanumeric_auth_token_gen] [tag:auth_token_length]
    fn gen_auth_token(&self) -> SmolStr {
        let mut rng = rand::thread_rng();
        let mut raw = gen_rand_arr::<_, 22>(&mut rng);
        let mut token = unsafe { std::str::from_utf8_unchecked(&raw) };
        while self.valid_sessions.contains_key(token) {
            raw = gen_rand_arr::<_, 22>(&mut rng);
            token = unsafe { std::str::from_utf8_unchecked(&raw) };
        }
        SmolStr::new_inline(token)
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
}

#[harmony_rust_sdk::api::exports::hrpc::async_trait]
impl auth_service_server::AuthService for AuthServer {
    type Error = ServerError;

    #[rate(20, 5)]
    async fn check_logged_in(
        &self,
        request: Request<CheckLoggedInRequest>,
    ) -> Result<CheckLoggedInResponse, HrpcServerError<Self::Error>> {
        auth!();
        Ok(CheckLoggedInResponse {})
    }

    #[rate(3, 1)]
    async fn federate(
        &self,
        request: Request<FederateRequest>,
    ) -> Result<FederateResponse, HrpcServerError<Self::Error>> {
        auth!();

        let keys_manager = self.keys_manager()?;

        let profile = self.profile_tree.get_profile_logic(user_id)?;
        let target = request.into_parts().0.into_message().await??.target;

        self.is_host_allowed(&target)?;

        let data = TokenData {
            user_id,
            target,
            username: profile.user_name,
            avatar: profile.user_avatar,
        };

        let token = keys_manager.generate_token(data).await?;

        Ok(FederateResponse { token: Some(token) })
    }

    #[rate(1, 5)]
    async fn login_federated(
        &self,
        request: Request<LoginFederatedRequest>,
    ) -> Result<LoginFederatedResponse, HrpcServerError<Self::Error>> {
        let LoginFederatedRequest { auth_token, domain } =
            request.into_parts().0.into_message().await??;

        self.is_host_allowed(&domain)?;

        if let Some(token) = auth_token {
            let keys_manager = self.keys_manager()?;
            let pubkey = keys_manager.get_key(domain.into()).await?;
            key::verify_token(&token, &pubkey)?;
            let TokenData {
                user_id: foreign_id,
                target,
                username,
                avatar,
            } = TokenData::decode(token.data.as_slice())
                .map_err(|_| ServerError::InvalidTokenData)?;

            let local_user_id = self
                .profile_tree
                .foreign_to_local_id(foreign_id, &target)
                .unwrap_or_else(|| {
                    let local_id = gen_rand_u64();

                    let mut batch = Batch::default();
                    // Add the local to foreign user key entry
                    batch.insert(
                        &make_local_to_foreign_user_key(local_id),
                        [&foreign_id.to_be_bytes(), target.as_bytes()].concat(),
                    );
                    // Add the foreign to local user key entry
                    batch.insert(
                        make_foreign_to_local_user_key(foreign_id, &target),
                        &local_id.to_be_bytes(),
                    );
                    // Add the profile entry
                    let profile = Profile {
                        is_bot: false,
                        user_status: UserStatus::OfflineUnspecified.into(),
                        user_avatar: avatar,
                        user_name: username,
                    };
                    let buf = encode_protobuf_message(profile);
                    batch.insert(&make_user_profile_key(local_id), buf.to_vec());
                    self.profile_tree.inner.apply_batch(batch).unwrap();

                    local_id
                });

            let session_token = self.gen_auth_token();
            let session = Session {
                session_token: session_token.to_string(),
                user_id: local_user_id,
            };
            self.valid_sessions.insert(session_token, local_user_id);

            return Ok(LoginFederatedResponse {
                session: Some(session),
            });
        }

        Err(ServerError::InvalidToken.into())
    }

    #[rate(1, 5)]
    async fn key(
        &self,
        _: Request<KeyRequest>,
    ) -> Result<KeyResponse, HrpcServerError<Self::Error>> {
        let keys_manager = self.keys_manager()?;
        let key = keys_manager.get_own_key().await?;

        Ok(KeyResponse {
            key: key.pk.to_vec(),
        })
    }

    fn stream_steps_on_upgrade(&self, response: Response) -> Response {
        set_proto_name(response)
    }

    type StreamStepsValidationType = SmolStr;

    async fn stream_steps_validation(
        &self,
        request: Request<Option<StreamStepsRequest>>,
    ) -> Result<SmolStr, HrpcServerError<Self::Error>> {
        if let Some(msg) = request.into_parts().0.into_optional_message().await?? {
            let auth_id = msg.auth_id;

            if self.step_map.contains_key(auth_id.as_str()) {
                Ok(auth_id.into())
            } else {
                Err(ServerError::InvalidAuthId.into())
            }
        } else {
            Ok(SmolStr::new_inline(""))
        }
    }

    #[rate(2, 5)]
    async fn stream_steps(&self, auth_id: SmolStr, mut socket: WriteSocket<StreamStepsResponse>) {
        let (tx, mut rx) = mpsc::channel(64);
        self.send_step.insert(auth_id.clone(), tx);
        while let Some(step) = rx.recv().await {
            if let Err(err) = socket
                .send_message(StreamStepsResponse { step: Some(step) })
                .await
            {
                tracing::error!("error occured: {}", err);

                // Break from loop since we errored
                break;
            }
        }
        self.send_step.remove(&auth_id);
    }

    #[rate(2, 5)]
    async fn begin_auth(
        &self,
        _: Request<BeginAuthRequest>,
    ) -> Result<BeginAuthResponse, HrpcServerError<Self::Error>> {
        let initial_step = AuthStep {
            can_go_back: false,
            fallback_url: String::default(),
            step: Some(auth_step::Step::Choice(auth_step::Choice {
                title: "initial".to_string(),
                options: ["login", "register"]
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            })),
        };

        let auth_id = gen_rand_inline_str();

        // [tag:step_stack_non_empty]
        self.step_map
            .entry(auth_id.clone())
            .and_modify(|s| *s = vec![initial_step.clone()])
            .or_insert_with(|| vec![initial_step.clone()]);

        tracing::debug!("new auth session {}", auth_id);

        Ok(BeginAuthResponse {
            auth_id: auth_id.into(),
        })
    }

    #[rate(10, 5)]
    async fn next_step(
        &self,
        req: Request<NextStepRequest>,
    ) -> Result<NextStepResponse, HrpcServerError<Self::Error>> {
        let NextStepRequest {
            auth_id,
            step: maybe_step,
        } = req.into_parts().0.into_message().await??;

        let next_step;

        if let Some(mut step_stack) = self.step_map.get_mut(auth_id.as_str()) {
            if let Some(step) = maybe_step {
                // Safety: step stack can never be empty, and our steps always have an inner step contained in them [ref:step_stack_non_empty]
                let current_step = unsafe {
                    step_stack
                        .last()
                        .unwrap_unchecked()
                        .step
                        .as_ref()
                        .unwrap_unchecked()
                        .clone()
                };
                tracing::debug!("current auth step for session {}", auth_id);
                tracing::debug!("client replied with {:#?}", step);
                match step {
                    next_step_request::Step::Choice(next_step_request::Choice { choice }) => {
                        if let auth_step::Step::Choice(auth_step::Choice { options, .. }) =
                            current_step
                        {
                            if options.contains(&choice) {
                                match choice.as_str() {
                                    "login" => {
                                        next_step = AuthStep {
                                            can_go_back: true,
                                            fallback_url: String::default(),
                                            step: Some(auth_step::Step::Form(auth_step::Form {
                                                title: "login".to_string(),
                                                fields: vec![
                                                    auth_step::form::FormField {
                                                        name: "email".to_string(),
                                                        r#type: "email".to_string(),
                                                    },
                                                    auth_step::form::FormField {
                                                        name: "password".to_string(),
                                                        r#type: "password".to_string(),
                                                    },
                                                ],
                                            })),
                                        };
                                        step_stack.push(next_step.clone());
                                    }
                                    "register" => {
                                        next_step = AuthStep {
                                            can_go_back: true,
                                            fallback_url: String::default(),
                                            step: Some(auth_step::Step::Form(auth_step::Form {
                                                title: "register".to_string(),
                                                fields: vec![
                                                    auth_step::form::FormField {
                                                        name: "username".to_string(),
                                                        r#type: "text".to_string(),
                                                    },
                                                    auth_step::form::FormField {
                                                        name: "email".to_string(),
                                                        r#type: "email".to_string(),
                                                    },
                                                    auth_step::form::FormField {
                                                        name: "password".to_string(),
                                                        r#type: "password".to_string(),
                                                    },
                                                ],
                                            })),
                                        };
                                        step_stack.push(next_step.clone());
                                    }
                                    _ => unreachable!(),
                                }
                            } else {
                                return Err(ServerError::NoSuchChoice {
                                    choice: choice.into(),
                                    expected_any_of: options.into_iter().map(Into::into).collect(),
                                }
                                .into());
                            }
                        } else {
                            return Err(ServerError::WrongStep {
                                expected: SmolStr::new_inline("form"),
                                got: SmolStr::new_inline("choice"),
                            }
                            .into());
                        }
                    }
                    next_step_request::Step::Form(next_step_request::Form { fields }) => {
                        if let auth_step::Step::Form(auth_step::Form {
                            fields: auth_fields,
                            title,
                        }) = current_step
                        {
                            use next_step_request::form_fields::Field;

                            let mut values = Vec::with_capacity(fields.len());

                            for (index, field) in fields.into_iter().enumerate() {
                                if let Some(afield) = auth_fields.get(index) {
                                    if let Some(field) = field.field {
                                        match afield.r#type.as_str() {
                                            "password" | "new-password" => {
                                                if matches!(field, Field::Bytes(_)) {
                                                    values.push(field);
                                                } else {
                                                    return Err(ServerError::WrongTypeForField {
                                                        name: afield.name.as_str().into(),
                                                        expected: SmolStr::new_inline("bytes"),
                                                    }
                                                    .into());
                                                }
                                            }
                                            "text" => {
                                                if matches!(field, Field::String(_)) {
                                                    values.push(field);
                                                } else {
                                                    return Err(ServerError::WrongTypeForField {
                                                        name: afield.name.as_str().into(),
                                                        expected: SmolStr::new_inline("text"),
                                                    }
                                                    .into());
                                                }
                                            }
                                            "number" => {
                                                if matches!(field, Field::Number(_)) {
                                                    values.push(field);
                                                } else {
                                                    return Err(ServerError::WrongTypeForField {
                                                        name: afield.name.as_str().into(),
                                                        expected: SmolStr::new_inline("number"),
                                                    }
                                                    .into());
                                                }
                                            }
                                            "email" => {
                                                if matches!(field, Field::String(_)) {
                                                    // TODO: validate email here and return error if invalid
                                                    values.push(field);
                                                } else {
                                                    return Err(ServerError::WrongTypeForField {
                                                        name: afield.name.as_str().into(),
                                                        expected: SmolStr::new_inline("email"),
                                                    }
                                                    .into());
                                                }
                                            }
                                            _ => unreachable!(),
                                        }
                                    } else {
                                        return Err(ServerError::NoFieldSpecified.into());
                                    }
                                } else {
                                    return Err(ServerError::NoSuchField.into());
                                }
                            }

                            match title.as_str() {
                                "login" => {
                                    let password_raw = try_get_password(&mut values)?;
                                    let password_hashed = hash_password(password_raw);
                                    let email = try_get_email(&mut values)?;

                                    let user_id = if let Some(user_id) =
                                        self.auth_tree.get(email.as_bytes()).unwrap()
                                    {
                                        // Safety: this unwrap can never cause UB since we only store u64
                                        u64::from_be_bytes(unsafe {
                                            user_id.try_into().unwrap_unchecked()
                                        })
                                    } else {
                                        return Err(ServerError::WrongUserOrPassword {
                                            email: email.into(),
                                        }
                                        .into());
                                    };

                                    if self
                                        .auth_tree
                                        .get(user_id.to_be_bytes().as_ref())
                                        .unwrap()
                                        .map_or(true, |pass| pass != password_hashed.as_ref())
                                    {
                                        return Err(ServerError::WrongUserOrPassword {
                                            email: email.into(),
                                        }
                                        .into());
                                    }

                                    let session_token = self.gen_auth_token(); // [ref:alphanumeric_auth_token_gen] [ref:auth_token_length]
                                    let mut batch = Batch::default();
                                    // [ref:token_u64_key]
                                    batch.insert(&token_key(user_id), session_token.as_str());
                                    batch.insert(
                                        // [ref:atime_u64_key]
                                        &atime_key(user_id),
                                        // [ref:atime_u64_value]
                                        &get_time_secs().to_be_bytes(),
                                    );
                                    self.auth_tree.apply_batch(batch).unwrap();

                                    tracing::debug!(
                                        "user {} logged in with email {}",
                                        user_id,
                                        email,
                                    );

                                    next_step = AuthStep {
                                        can_go_back: false,
                                        fallback_url: String::default(),
                                        step: Some(auth_step::Step::Session(Session {
                                            user_id,
                                            session_token: session_token.clone().into(),
                                        })),
                                    };

                                    self.valid_sessions.insert(session_token, user_id);
                                }
                                "register" => {
                                    let password_raw = try_get_password(&mut values)?;
                                    let password_hashed = hash_password(password_raw);
                                    let email = try_get_email(&mut values)?;
                                    let username = try_get_username(&mut values)?;

                                    if self.auth_tree.get(email.as_bytes()).unwrap().is_some() {
                                        return Err(ServerError::UserAlreadyExists.into());
                                    }

                                    let user_id = gen_rand_u64();
                                    let session_token = self.gen_auth_token(); // [ref:alphanumeric_auth_token_gen] [ref:auth_token_length]

                                    let mut batch = Batch::default();
                                    batch.insert(email.as_str(), &user_id.to_be_bytes());
                                    batch.insert(&user_id.to_be_bytes(), password_hashed.as_ref());
                                    // [ref:token_u64_key]
                                    batch.insert(&token_key(user_id), session_token.as_str());
                                    batch.insert(
                                        // [ref:atime_u64_key]
                                        &atime_key(user_id),
                                        // [ref:atime_u64_value]
                                        &get_time_secs().to_be_bytes(),
                                    );
                                    self.auth_tree
                                        .apply_batch(batch)
                                        .expect("failed to register into db");

                                    let buf = encode_protobuf_message(Profile {
                                        user_name: username,
                                        ..Default::default()
                                    });
                                    profile_insert!(make_user_profile_key(user_id) / buf);

                                    tracing::debug!(
                                        "new user {} registered with email {}",
                                        user_id,
                                        email,
                                    );

                                    next_step = AuthStep {
                                        can_go_back: false,
                                        fallback_url: String::default(),
                                        step: Some(auth_step::Step::Session(Session {
                                            user_id,
                                            session_token: session_token.clone().into(),
                                        })),
                                    };

                                    self.valid_sessions.insert(session_token, user_id);
                                }
                                _ => unreachable!(),
                            }
                        } else {
                            return Err(ServerError::WrongStep {
                                expected: SmolStr::new_inline("choice"),
                                got: SmolStr::new_inline("form"),
                            }
                            .into());
                        }
                    }
                }
            } else {
                // Safety: step stack can never be empty [ref:step_stack_non_empty]
                next_step = unsafe { step_stack.last().unwrap_unchecked().clone() };
            }
        } else {
            return Err(ServerError::InvalidAuthId.into());
        }

        if let Some(chan) = self.send_step.get(auth_id.as_str()) {
            tracing::debug!("sending next step to {} stream", auth_id);
            if let Err(err) = chan.send(next_step.clone()).await {
                tracing::error!("failed to send auth step to {}: {}", auth_id, err);
            }
        }

        if let Some(auth_step::Step::Session(session)) = &next_step.step {
            tracing::debug!(
                "auth session {} complete with session {:#?}",
                auth_id,
                session
            );
            self.step_map.remove(auth_id.as_str());
        }

        Ok(NextStepResponse {
            step: Some(next_step),
        })
    }

    #[rate(10, 5)]
    async fn step_back(
        &self,
        req: Request<StepBackRequest>,
    ) -> Result<StepBackResponse, HrpcServerError<Self::Error>> {
        let req = req.into_parts().0.into_message().await??;
        let auth_id = req.auth_id;

        let prev_step;

        if let Some(mut step_stack) = self.step_map.get_mut(auth_id.as_str()) {
            // Safety: step stack can never be empty [ref:step_stack_non_empty]
            if unsafe { step_stack.last().unwrap_unchecked().can_go_back } {
                step_stack.pop();
                tracing::debug!("auth session {} went to previous step", auth_id);
            } else {
                tracing::debug!(
                    "auth session {} wanted prev step, but we can't go back",
                    auth_id
                );
            }
            // Safety: step stack can never be empty [ref:step_stack_non_empty]
            prev_step = unsafe { step_stack.last().unwrap_unchecked().clone() };
            if let Some(chan) = self.send_step.get(auth_id.as_str()) {
                tracing::debug!("sending prev step to {} stream", auth_id);
                if let Err(err) = chan.send(prev_step.clone()).await {
                    tracing::error!("failed to send auth step to {}: {}", auth_id, err);
                }
            }
        } else {
            return Err(ServerError::InvalidAuthId.into());
        }

        Ok(StepBackResponse {
            step: Some(prev_step),
        })
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

#[inline(always)]
fn try_get_string(values: &mut Vec<Field>, err: ServerError) -> ServerResult<String> {
    if let Some(Field::String(value)) = values.pop() {
        Ok(value)
    } else {
        Err(err)
    }
}

#[inline(always)]
fn try_get_bytes(values: &mut Vec<Field>, err: ServerError) -> ServerResult<Vec<u8>> {
    if let Some(Field::Bytes(value)) = values.pop() {
        Ok(value)
    } else {
        Err(err)
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
