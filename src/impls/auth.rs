use std::{collections::HashMap, convert::TryInto, sync::Arc, time::Instant};

use harmony_rust_sdk::api::{
    auth::*,
    chat::GetUserResponse,
    exports::{
        hrpc::{encode_protobuf_message, Request},
        prost::bytes::BytesMut,
    },
};
use parking_lot::Mutex;
use sled::{IVec, Tree};

use super::{gen_rand_str, gen_rand_u64};
use crate::{
    db::{auth::*, chat::make_member_profile_key},
    ServerError,
};

const SESSION_EXPIRE: u64 = 60 * 60 * 24 * 2;

#[derive(Debug)]
pub struct AuthServer {
    valid_sessions: Arc<Mutex<HashMap<String, u64>>>,
    step_map: Mutex<HashMap<String, Vec<AuthStep>>>,
    send_step: Mutex<HashMap<String, AuthStep>>,
    auth_tree: Tree,
    chat_tree: Tree,
}

impl AuthServer {
    pub fn new(
        chat_tree: Tree,
        auth_tree: Tree,
        valid_sessions: Arc<Mutex<HashMap<String, u64>>>,
    ) -> Self {
        fn scan_tree_for(auth_tree: &Tree, prefix: &[u8]) -> Vec<(u64, IVec)> {
            auth_tree
                .scan_prefix(prefix)
                .flatten()
                .map(|(key, val)| {
                    (
                        u64::from_be_bytes(key.split_at(prefix.len()).1.try_into().unwrap()),
                        val,
                    )
                })
                .collect()
        }

        let tokens = scan_tree_for(&auth_tree, &TOKEN_PREFIX);
        let atimes = scan_tree_for(&auth_tree, &ATIME_PREFIX);

        let mut batch = sled::Batch::default();
        let mut vs = valid_sessions.lock();
        for (id, token) in tokens {
            for (oid, atime) in &atimes {
                if &id == oid {
                    let secs = u64::from_be_bytes(atime.as_ref().try_into().unwrap());
                    let auth_how_old = Instant::now().elapsed().as_secs() - secs;

                    if auth_how_old >= SESSION_EXPIRE {
                        tracing::info!("user {} session has expired", id);
                        batch.remove(&token_key(id));
                        batch.remove(&atime_key(id));
                    } else {
                        let token = std::str::from_utf8(token.as_ref()).unwrap();
                        vs.insert(token.to_string(), id);
                    }
                }
            }
        }
        drop(vs);
        auth_tree.apply_batch(batch).unwrap();

        let att = auth_tree.clone();
        let vs = valid_sessions.clone();

        std::thread::spawn(move || {
            tracing::info!("starting auth session expiration check thread");
            let mut since = Instant::now();
            loop {
                if since.elapsed().as_secs() > 60 * 5 {
                    let tokens = scan_tree_for(&att, &TOKEN_PREFIX);
                    let atimes = scan_tree_for(&att, &ATIME_PREFIX);

                    let mut batch = sled::Batch::default();
                    let mut vs = vs.lock();
                    for (id, token) in tokens {
                        for (oid, atime) in &atimes {
                            if &id == oid {
                                let secs = u64::from_be_bytes(atime.as_ref().try_into().unwrap());
                                let auth_how_old = Instant::now().elapsed().as_secs() - secs;

                                if auth_how_old >= SESSION_EXPIRE {
                                    tracing::info!("user {} session has expired", id);
                                    batch.remove(&token_key(id));
                                    batch.remove(&atime_key(id));
                                    let token = std::str::from_utf8(token.as_ref()).unwrap();
                                    vs.remove(token);
                                }
                            }
                        }
                    }
                    drop(vs);
                    att.apply_batch(batch).unwrap();

                    since = Instant::now();
                }
            }
        });

        Self {
            valid_sessions,
            step_map: Mutex::new(HashMap::new()),
            send_step: Mutex::new(HashMap::new()),
            auth_tree,
            chat_tree,
        }
    }
}

#[harmony_rust_sdk::api::exports::hrpc::async_trait]
impl auth_service_server::AuthService for AuthServer {
    type Error = ServerError;

    async fn federate(&self, _: Request<FederateRequest>) -> Result<FederateReply, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn login_federated(
        &self,
        _: Request<LoginFederatedRequest>,
    ) -> Result<Session, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn key(&self, _: Request<()>) -> Result<KeyReply, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn stream_steps(
        &self,
        validation_request: &Request<StreamStepsRequest>,
    ) -> Result<Option<AuthStep>, Self::Error> {
        let auth_id = &validation_request.get_message().auth_id;

        if !self.step_map.lock().contains_key(auth_id) {
            return Err(ServerError::InvalidAuthId);
        }

        Ok(self.send_step.lock().remove(auth_id))
    }

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

        let auth_id: String = gen_rand_str(30);

        self.step_map
            .lock()
            .entry(auth_id.clone())
            .and_modify(|s| *s = vec![initial_step.clone()])
            .or_insert_with(|| vec![initial_step.clone()]);

        self.send_step.lock().insert(auth_id.clone(), initial_step);

        tracing::debug!("new auth session {}", auth_id);

        Ok(BeginAuthResponse { auth_id })
    }

    async fn next_step(&self, req: Request<NextStepRequest>) -> Result<AuthStep, Self::Error> {
        let NextStepRequest {
            auth_id,
            step: maybe_step,
        } = req.into_parts().0;

        let next_step;

        if let Some(step_stack) = self.step_map.lock().get_mut(&auth_id) {
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
                                    choice,
                                    expected_any_of: options,
                                });
                            }
                        } else {
                            return Err(ServerError::WrongStep {
                                expected: "form".to_string(),
                                got: "choice".to_string(),
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
                                                        name: afield.name.clone(),
                                                        expected: "bytes".to_string(),
                                                    });
                                                }
                                            }
                                            "text" => {
                                                if matches!(field, Field::String(_)) {
                                                    values.push(field);
                                                } else {
                                                    return Err(ServerError::WrongTypeForField {
                                                        name: afield.name.clone(),
                                                        expected: "text".to_string(),
                                                    });
                                                }
                                            }
                                            "number" => {
                                                if matches!(field, Field::Number(_)) {
                                                    values.push(field);
                                                } else {
                                                    return Err(ServerError::WrongTypeForField {
                                                        name: afield.name.clone(),
                                                        expected: "number".to_string(),
                                                    });
                                                }
                                            }
                                            "email" => {
                                                if matches!(field, Field::String(_)) {
                                                    // TODO: validate email here and return error if invalid
                                                    values.push(field);
                                                } else {
                                                    return Err(ServerError::WrongTypeForField {
                                                        name: afield.name.clone(),
                                                        expected: "email".to_string(),
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
                                    let password = if let Some(Field::Bytes(value)) = values.pop() {
                                        value
                                    } else {
                                        return Err(ServerError::WrongTypeForField {
                                            name: "password".to_string(),
                                            expected: "bytes".to_string(),
                                        });
                                    };

                                    let email = if let Some(Field::String(value)) = values.pop() {
                                        value
                                    } else {
                                        return Err(ServerError::WrongTypeForField {
                                            name: "email".to_string(),
                                            expected: "string".to_string(),
                                        });
                                    };

                                    let user_id =
                                        if let Ok(Some(user_id)) = self.auth_tree.get(&email) {
                                            u64::from_be_bytes(
                                                user_id
                                                    .split_at(std::mem::size_of::<u64>())
                                                    .0
                                                    .try_into()
                                                    .unwrap(),
                                            )
                                        } else {
                                            return Err(ServerError::WrongUserOrPassword { email });
                                        };

                                    if let Ok(Some(pass)) =
                                        self.auth_tree.get(user_id.to_be_bytes())
                                    {
                                        // TODO: actually validate password properly lol
                                        if pass != password {
                                            return Err(ServerError::WrongUserOrPassword { email });
                                        }
                                    } else {
                                        return Err(ServerError::WrongUserOrPassword { email });
                                    }

                                    let session_token = gen_rand_str(30);
                                    let mut batch = sled::Batch::default();
                                    batch.insert(&token_key(user_id), session_token.as_str());
                                    batch.insert(
                                        &atime_key(user_id),
                                        &Instant::now().elapsed().as_secs().to_be_bytes(),
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
                                            session_token: session_token.clone(),
                                        })),
                                    };

                                    self.valid_sessions.lock().insert(session_token, user_id);
                                }
                                "register" => {
                                    let password = if let Some(Field::Bytes(value)) = values.pop() {
                                        value
                                    } else {
                                        return Err(ServerError::WrongTypeForField {
                                            name: "password".to_string(),
                                            expected: "bytes".to_string(),
                                        });
                                    };

                                    let email = if let Some(Field::String(value)) = values.pop() {
                                        value
                                    } else {
                                        return Err(ServerError::WrongTypeForField {
                                            name: "email".to_string(),
                                            expected: "email".to_string(),
                                        });
                                    };

                                    let username = if let Some(Field::String(value)) = values.pop()
                                    {
                                        value
                                    } else {
                                        return Err(ServerError::WrongTypeForField {
                                            name: "username".to_string(),
                                            expected: "text".to_string(),
                                        });
                                    };

                                    if self.auth_tree.get(&email).unwrap().is_some() {
                                        return Err(ServerError::UserAlreadyExists);
                                    }

                                    let user_id = gen_rand_u64();
                                    let session_token = gen_rand_str(30);

                                    let mut batch = sled::Batch::default();
                                    batch.insert(email.as_str(), &user_id.to_be_bytes());
                                    batch.insert(&user_id.to_be_bytes(), password);
                                    batch.insert(&token_key(user_id), session_token.as_str());
                                    batch.insert(
                                        &atime_key(user_id),
                                        &Instant::now().elapsed().as_secs().to_be_bytes(),
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
                                        .insert(make_member_profile_key(user_id), buf.as_ref())
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
                                            session_token: session_token.clone(),
                                        })),
                                    };

                                    self.valid_sessions.lock().insert(session_token, user_id);
                                }
                                _ => unreachable!(),
                            }
                        } else {
                            return Err(ServerError::WrongStep {
                                expected: "choice".to_string(),
                                got: "form".to_string(),
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

        if let Some(auth_step::Step::Session(session)) = &next_step.step {
            tracing::debug!(
                "auth session {} complete with session {:#?}",
                auth_id,
                session
            );
            self.step_map.lock().remove(&auth_id);
        }

        self.send_step.lock().insert(auth_id, next_step.clone());

        Ok(next_step)
    }

    async fn step_back(&self, req: Request<StepBackRequest>) -> Result<AuthStep, Self::Error> {
        let req = req.into_parts().0;
        let auth_id = req.auth_id;

        let prev_step;

        if let Some(step_stack) = self.step_map.lock().get_mut(&auth_id) {
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
            self.send_step.lock().insert(auth_id, prev_step.clone());
        } else {
            return Err(ServerError::InvalidAuthId);
        }

        Ok(prev_step)
    }
}
