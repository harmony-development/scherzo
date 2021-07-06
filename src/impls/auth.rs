use std::{convert::TryInto, mem::size_of, sync::Arc, time::Duration};

use ahash::RandomState;
use dashmap::DashMap;
use harmony_rust_sdk::api::{
    auth::*,
    chat::GetUserResponse,
    exports::{
        hrpc::{
            encode_protobuf_message, return_print, server::WriteSocket, warp::reply::Response,
            Request,
        },
        prost::bytes::BytesMut,
    },
};
use scherzo_derive::rate;
use sha3::Digest;
use sled::{IVec, Tree};
use smol_str::SmolStr;
use tokio::sync::mpsc::{self, Sender};

use crate::{
    db::{
        self,
        auth::*,
        chat::{self as chatdb, make_user_profile_key},
    },
    http,
    impls::{gen_rand_inline_str, gen_rand_u64, get_time_secs},
    set_proto_name, ServerError, ServerResult,
};

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
                    .map(|val| {
                        val.to_str()
                            .ok()
                            .map(|v| v.split(',').nth(1).map(str::trim))
                            .flatten()
                    })
                    .flatten()
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

#[derive(Debug)]
pub struct AuthServer {
    valid_sessions: SessionMap,
    step_map: DashMap<SmolStr, Vec<AuthStep>, RandomState>,
    send_step: DashMap<SmolStr, Sender<AuthStep>, RandomState>,
    auth_tree: Tree,
    chat_tree: Tree,
}

impl AuthServer {
    pub fn new(chat_tree: Tree, auth_tree: Tree, valid_sessions: SessionMap) -> Self {
        let att = auth_tree.clone();
        let ctt = chat_tree.clone();
        let vs = valid_sessions.clone();

        std::thread::spawn(move || {
            let _guard = tracing::info_span!("auth_session_check").entered();
            tracing::info!("starting auth session expiration check thread");

            fn scan_tree_for(att: &Tree, prefix: &[u8]) -> Vec<(u64, IVec)> {
                let len = prefix.len();
                att.scan_prefix(prefix)
                    .map(move |res| {
                        let (key, val) = res.unwrap();
                        (
                            u64::from_be_bytes(key.split_at(len).1.try_into().unwrap()),
                            val,
                        )
                    })
                    .collect()
            }

            loop {
                let tokens = scan_tree_for(&att, TOKEN_PREFIX);
                let atimes = scan_tree_for(&att, ATIME_PREFIX);

                let mut batch = sled::Batch::default();
                for (id, raw_token) in tokens {
                    if let Some(profile) = ctt.get(chatdb::make_user_profile_key(id)).unwrap() {
                        let profile = db::deser_profile(profile);
                        for (oid, raw_atime) in &atimes {
                            if id.eq(oid) {
                                let secs =
                                    u64::from_be_bytes(raw_atime.as_ref().try_into().unwrap());
                                let auth_how_old = get_time_secs() - secs;
                                // Safety: all of our tokens are valid str's, we never generate invalid ones [ref:alphanumeric_auth_token_gen]
                                let token =
                                    unsafe { std::str::from_utf8_unchecked(raw_token.as_ref()) };

                                if vs.contains_key(token) {
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
            valid_sessions,
            step_map: DashMap::default(),
            send_step: DashMap::default(),
            auth_tree,
            chat_tree,
        }
    }

    #[inline(always)]
    fn auth<T>(&self, request: &Request<T>) -> Result<u64, ServerError> {
        check_auth(&self.valid_sessions, request)
    }
}

#[harmony_rust_sdk::api::exports::hrpc::async_trait]
impl auth_service_server::AuthService for AuthServer {
    type Error = ServerError;

    #[rate(20, 5)]
    async fn check_logged_in(&self, request: Request<()>) -> Result<(), Self::Error> {
        self.auth(&request).map(|_| ())
    }

    #[rate(3, 1)]
    async fn federate(&self, _: Request<FederateRequest>) -> Result<FederateReply, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    #[rate(1, 5)]
    async fn login_federated(
        &self,
        _: Request<LoginFederatedRequest>,
    ) -> Result<Session, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    #[rate(1, 5)]
    async fn key(&self, _: Request<()>) -> Result<KeyReply, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    fn stream_steps_on_upgrade(&self, response: Response) -> Response {
        set_proto_name(response)
    }

    type StreamStepsValidationType = SmolStr;

    async fn stream_steps_validation(
        &self,
        request: Request<Option<StreamStepsRequest>>,
    ) -> Result<SmolStr, Self::Error> {
        if let Some(msg) = request.into_parts().0 {
            let auth_id = msg.auth_id;

            if self.step_map.contains_key(auth_id.as_str()) {
                Ok(auth_id.into())
            } else {
                Err(ServerError::InvalidAuthId)
            }
        } else {
            Ok(SmolStr::default())
        }
    }

    #[rate(2, 5)]
    async fn stream_steps(&self, auth_id: SmolStr, mut socket: WriteSocket<AuthStep>) {
        let (tx, mut rx) = mpsc::channel(64);
        self.send_step.insert(auth_id, tx);
        loop {
            if let Some(step) = rx.recv().await {
                return_print!(socket.send_message(step).await);
            }
        }
    }

    #[rate(2, 5)]
    async fn begin_auth(&self, _: Request<()>) -> Result<BeginAuthResponse, Self::Error> {
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
    async fn next_step(&self, req: Request<NextStepRequest>) -> Result<AuthStep, Self::Error> {
        let NextStepRequest {
            auth_id,
            step: maybe_step,
        } = req.into_parts().0;

        let next_step;

        if let Some(mut step_stack) = self.step_map.get_mut(auth_id.as_str()) {
            if let Some(step) = maybe_step {
                let current_step = step_stack.last().unwrap().step.as_ref().unwrap().clone();
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
                                });
                            }
                        } else {
                            return Err(ServerError::WrongStep {
                                expected: SmolStr::new_inline("form"),
                                got: SmolStr::new_inline("choice"),
                            });
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
                                                    });
                                                }
                                            }
                                            "text" => {
                                                if matches!(field, Field::String(_)) {
                                                    values.push(field);
                                                } else {
                                                    return Err(ServerError::WrongTypeForField {
                                                        name: afield.name.as_str().into(),
                                                        expected: SmolStr::new_inline("text"),
                                                    });
                                                }
                                            }
                                            "number" => {
                                                if matches!(field, Field::Number(_)) {
                                                    values.push(field);
                                                } else {
                                                    return Err(ServerError::WrongTypeForField {
                                                        name: afield.name.as_str().into(),
                                                        expected: SmolStr::new_inline("number"),
                                                    });
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
                                                    });
                                                }
                                            }
                                            _ => unreachable!(),
                                        }
                                    } else {
                                        return Err(ServerError::NoFieldSpecified);
                                    }
                                } else {
                                    return Err(ServerError::NoSuchField);
                                }
                            }

                            match title.as_str() {
                                "login" => {
                                    let password_raw = try_get_password(&mut values)?;
                                    let password_hashed = hash_password(password_raw);
                                    let email = try_get_email(&mut values)?;

                                    let user_id =
                                        if let Ok(Some(user_id)) = self.auth_tree.get(&email) {
                                            u64::from_be_bytes(
                                                user_id
                                                    .split_at(size_of::<u64>())
                                                    .0
                                                    .try_into()
                                                    .unwrap(),
                                            )
                                        } else {
                                            return Err(ServerError::WrongUserOrPassword {
                                                email: email.into(),
                                            });
                                        };

                                    if self
                                        .auth_tree
                                        .get(user_id.to_be_bytes())
                                        .unwrap()
                                        .map_or(true, |pass| pass != password_hashed.as_ref())
                                    {
                                        return Err(ServerError::WrongUserOrPassword {
                                            email: email.into(),
                                        });
                                    }

                                    let session_token = gen_auth_token(); // [ref:alphanumeric_auth_token_gen] [ref:auth_token_length]
                                    let mut batch = sled::Batch::default();
                                    batch.insert(&token_key(user_id), session_token.as_str());
                                    batch.insert(
                                        &atime_key(user_id),
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

                                    if self.auth_tree.get(&email).unwrap().is_some() {
                                        return Err(ServerError::UserAlreadyExists);
                                    }

                                    let user_id = gen_rand_u64();
                                    let session_token = gen_auth_token(); // [ref:alphanumeric_auth_token_gen] [ref:auth_token_length]

                                    let mut batch = sled::Batch::default();
                                    batch.insert(email.as_str(), &user_id.to_be_bytes());
                                    batch.insert(&user_id.to_be_bytes(), password_hashed.as_ref());
                                    batch.insert(&token_key(user_id), session_token.as_str());
                                    batch.insert(
                                        &atime_key(user_id),
                                        &get_time_secs().to_be_bytes(),
                                    );
                                    self.auth_tree
                                        .apply_batch(batch)
                                        .expect("failed to register into db");

                                    let mut buf = BytesMut::new();
                                    encode_protobuf_message(
                                        &mut buf,
                                        GetUserResponse {
                                            user_name: username,
                                            ..Default::default()
                                        },
                                    );
                                    self.chat_tree
                                        .insert(make_user_profile_key(user_id), buf.as_ref())
                                        .unwrap();

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
                            });
                        }
                    }
                }
            } else {
                next_step = step_stack.last().unwrap().clone();
            }
        } else {
            return Err(ServerError::InvalidAuthId);
        }

        if let Some(chan) = self.send_step.get(auth_id.as_str()) {
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
            self.send_step.remove(auth_id.as_str());
        }

        Ok(next_step)
    }

    #[rate(10, 5)]
    async fn step_back(&self, req: Request<StepBackRequest>) -> Result<AuthStep, Self::Error> {
        let req = req.into_parts().0;
        let auth_id = req.auth_id;

        let prev_step;

        if let Some(mut step_stack) = self.step_map.get_mut(auth_id.as_str()) {
            if step_stack.last().unwrap().can_go_back {
                step_stack.pop();
                tracing::debug!("auth session {} went to previous step", auth_id);
            } else {
                tracing::debug!(
                    "auth session {} wanted prev step, but we can't go back",
                    auth_id
                );
            }
            prev_step = step_stack.last().unwrap().clone();
            if let Some(chan) = self.send_step.get(auth_id.as_str()) {
                if let Err(err) = chan.send(prev_step.clone()).await {
                    tracing::error!("failed to send auth step to {}: {}", auth_id, err);
                }
            }
        } else {
            return Err(ServerError::InvalidAuthId);
        }

        Ok(prev_step)
    }
}

fn hash_password(raw: Vec<u8>) -> impl AsRef<[u8]> {
    let mut sh = sha3::Sha3_512::new();
    sh.update(raw);
    sh.finalize()
}

#[inline(always)]
fn gen_auth_token() -> SmolStr {
    // [tag:alphanumeric_auth_token_gen] [tag:auth_token_length]
    gen_rand_inline_str() // generates 22 chars long inlined SmolStr [ref:inlined_smol_str_gen]
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

use next_step_request::form_fields::Field;

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
