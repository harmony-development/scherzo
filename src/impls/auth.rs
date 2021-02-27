use std::{collections::HashMap, convert::TryInto};

use harmony_rust_sdk::api::auth::*;
use parking_lot::Mutex;
use rand::Rng;
use sled::Db;

use crate::ServerError;

fn gen_rand_str(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(rand::distributions::Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

#[derive(Debug)]
pub struct AuthServer {
    step_map: Mutex<HashMap<String, Vec<AuthStep>>>,
    db: Db,
}

impl AuthServer {
    pub fn new(db: Db) -> Self {
        Self {
            step_map: Mutex::new(HashMap::new()),
            db,
        }
    }
}

#[hrpc::async_trait]
impl auth_service_server::AuthService for AuthServer {
    type Error = ServerError;

    async fn federate(&self, _: FederateRequest) -> Result<FederateReply, Self::Error> {
        todo!("federate")
    }

    async fn login_federated(&self, _: LoginFederatedRequest) -> Result<Session, Self::Error> {
        todo!("login federated")
    }

    async fn key(&self, _: ()) -> Result<KeyReply, Self::Error> {
        todo!("key")
    }

    async fn begin_auth(&self, _: ()) -> Result<BeginAuthResponse, Self::Error> {
        let initial_step = vec![AuthStep {
            can_go_back: false,
            fallback_url: String::default(),
            step: Some(auth_step::Step::Choice(auth_step::Choice {
                title: "initial".to_string(),
                options: ["login", "register"]
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            })),
        }];

        let auth_id: String = gen_rand_str(30);

        self.step_map
            .lock()
            .entry(auth_id.clone())
            .and_modify(|s| *s = initial_step.clone())
            .or_insert(initial_step);

        Ok(BeginAuthResponse { auth_id })
    }

    async fn next_step(&self, req: NextStepRequest) -> Result<AuthStep, Self::Error> {
        let NextStepRequest {
            auth_id,
            step: maybe_step,
        } = req;

        let next_step;

        if let Some(step_stack) = self.step_map.lock().get_mut(&auth_id) {
            if let Some(step) = maybe_step {
                match step {
                    next_step_request::Step::Choice(next_step_request::Choice { choice }) => {
                        let current_step =
                            step_stack.last().unwrap().step.as_ref().unwrap().clone();

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
                        let current_step =
                            step_stack.last().unwrap().step.as_ref().unwrap().clone();

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

                                    let auth_tree = self.db.open_tree("auth").unwrap();

                                    let user_id = if let Ok(Some(user_id)) = auth_tree.get(&email) {
                                        u64::from_le_bytes(
                                            user_id
                                                .split_at(std::mem::size_of::<u64>())
                                                .0
                                                .try_into()
                                                .unwrap(),
                                        )
                                    } else {
                                        return Err(ServerError::WrongUserOrPassword { email });
                                    };

                                    if let Ok(Some(pass)) = auth_tree.get(user_id.to_le_bytes()) {
                                        // TODO: actually validate password properly lol
                                        if pass != password {
                                            return Err(ServerError::WrongUserOrPassword { email });
                                        }
                                    } else {
                                        return Err(ServerError::WrongUserOrPassword { email });
                                    }

                                    let session_token = gen_rand_str(30);

                                    next_step = AuthStep {
                                        can_go_back: false,
                                        fallback_url: String::default(),
                                        step: Some(auth_step::Step::Session(Session {
                                            user_id,
                                            session_token,
                                        })),
                                    };
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

                                    let auth_tree = self.db.open_tree("auth").unwrap();

                                    if auth_tree.get(&email).unwrap().is_some() {
                                        return Err(ServerError::UserAlreadyExists);
                                    }

                                    let user_id: u64 = rand::thread_rng().gen_range(1..u64::MAX);

                                    let mut batch = sled::Batch::default();
                                    batch.insert(
                                        format!("{}_name", user_id).as_str(),
                                        username.as_str(),
                                    );
                                    batch.insert(email.as_str(), &user_id.to_le_bytes());
                                    batch.insert(&user_id.to_le_bytes(), password);
                                    auth_tree
                                        .apply_batch(batch)
                                        .expect("failed to register into db");

                                    let session_token = gen_rand_str(30);

                                    next_step = AuthStep {
                                        can_go_back: false,
                                        fallback_url: String::default(),
                                        step: Some(auth_step::Step::Session(Session {
                                            user_id,
                                            session_token,
                                        })),
                                    };
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

        if matches!(next_step.step, Some(auth_step::Step::Session(_))) {
            self.step_map.lock().remove(&auth_id);
        }

        Ok(next_step)
    }

    async fn step_back(&self, req: StepBackRequest) -> Result<AuthStep, Self::Error> {
        let auth_id = req.auth_id;

        let prev_step;

        if let Some(step_stack) = self.step_map.lock().get_mut(&auth_id) {
            if step_stack.last().unwrap().can_go_back {
                prev_step = step_stack.pop().unwrap();
            } else {
                prev_step = step_stack.last().unwrap().clone();
            }
        } else {
            return Err(ServerError::InvalidAuthId);
        }

        Ok(prev_step)
    }
}
