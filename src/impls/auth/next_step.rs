use super::*;

pub async fn handler(
    svc: &AuthServer,
    req: Request<NextStepRequest>,
) -> ServerResult<Response<NextStepResponse>> {
    let NextStepRequest {
        auth_id,
        step: maybe_step,
    } = req.into_message().await?;

    let auth_id: SmolStr = auth_id.into();

    tracing::debug!("got next step for auth id {}", auth_id);

    let next_step;

    let mut step_stack = svc
        .step_map
        .get_mut(auth_id.as_str())
        .ok_or(ServerError::InvalidAuthId)?;

    if let Some(step) = maybe_step {
        // Safety: step stack can never be empty, and our steps always have an
        // inner step contained in them [ref:step_stack_non_empty]
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
                let auth_step::Step::Choice(auth_step::Choice { options, .. }) = current_step else {
                    bail!(ServerError::WrongStep {
                        expected: SmolStr::new_inline("form"),
                        got: SmolStr::new_inline("choice"),
                    });
                };
                if !options.contains(&choice) {
                    bail!(ServerError::NoSuchChoice {
                        choice: choice.into(),
                        expected_any_of: options.into_iter().map(Into::into).collect(),
                    });
                }

                next_step = match choice.as_str() {
                    "login" => AuthStep {
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
                    },
                    "register" => {
                        let mut fields = vec![
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
                        ];
                        if svc.deps.config.policy.disable_registration {
                            fields.push(auth_step::form::FormField {
                                name: "token".to_string(),
                                r#type: "password".to_string(),
                            });
                        }
                        AuthStep {
                            can_go_back: true,
                            fallback_url: String::default(),
                            step: Some(auth_step::Step::Form(auth_step::Form {
                                title: "register".to_string(),
                                fields,
                            })),
                        }
                    }
                    _ => unreachable!(),
                };
                step_stack.push(next_step.clone());
            }
            next_step_request::Step::Form(next_step_request::Form { fields }) => {
                if let auth_step::Step::Form(auth_step::Form {
                    fields: auth_fields,
                    title,
                }) = current_step
                {
                    let mut values = Vec::with_capacity(fields.len());

                    for (index, field) in fields.into_iter().enumerate() {
                        let afield = auth_fields.get(index).ok_or(ServerError::NoSuchField)?;
                        let field = field.field.ok_or(ServerError::NoFieldSpecified)?;

                        match afield.r#type.as_str() {
                            "password" | "new-password" => {
                                if matches!(field, Field::Bytes(_)) {
                                    values.push(field);
                                } else {
                                    bail!(ServerError::WrongTypeForField {
                                        name: afield.name.as_str().into(),
                                        expected: SmolStr::new_inline("bytes"),
                                    });
                                }
                            }
                            "text" => {
                                if matches!(field, Field::String(_)) {
                                    values.push(field);
                                } else {
                                    bail!(ServerError::WrongTypeForField {
                                        name: afield.name.as_str().into(),
                                        expected: SmolStr::new_inline("text"),
                                    });
                                }
                            }
                            "number" => {
                                if matches!(field, Field::Number(_)) {
                                    values.push(field);
                                } else {
                                    bail!(ServerError::WrongTypeForField {
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
                                    bail!(ServerError::WrongTypeForField {
                                        name: afield.name.as_str().into(),
                                        expected: SmolStr::new_inline("email"),
                                    });
                                }
                            }
                            _ => unreachable!(),
                        }
                    }

                    match title.as_str() {
                        "login" => {
                            let password_raw = try_get_password(&mut values)?;
                            let password_hashed = hash_password(password_raw);
                            let email = try_get_email(&mut values)?;

                            let user_id = svc.deps.auth_tree.get(email.as_bytes())?.map_or_else(
                                || Err(ServerError::WrongUserOrPassword {
                                    email: email.as_str().into(),
                                }),
                                |raw| {
                                    // Safety: this unwrap can never cause UB since we only store u64
                                    Ok(u64::from_be_bytes(unsafe { raw.try_into().unwrap_unchecked() }))
                                },
                            )?;

                            if svc
                                .deps
                                .auth_tree
                                .get(user_id.to_be_bytes().as_ref())?
                                .map_or(true, |pass| pass != password_hashed.as_ref())
                            {
                                return Err(ServerError::WrongUserOrPassword {
                                    email: email.into(),
                                }
                                .into());
                            }

                            let session_token = svc.gen_auth_token(); // [ref:alphanumeric_auth_token_gen] [ref:auth_token_length]
                            let mut batch = Batch::default();
                            // [ref:token_u64_key]
                            batch.insert(
                                token_key(user_id).to_vec(),
                                session_token.as_str().as_bytes().to_vec(),
                            );
                            batch.insert(
                                // [ref:atime_u64_key]
                                atime_key(user_id).to_vec(),
                                // [ref:atime_u64_value]
                                get_time_secs().to_be_bytes().to_vec(),
                            );
                            svc.deps
                                .auth_tree
                                .inner
                                .apply_batch(batch)
                                .map_err(ServerError::DbError)?;

                            tracing::debug!("user {} logged in with email {}", user_id, email,);

                            next_step = AuthStep {
                                can_go_back: false,
                                fallback_url: String::default(),
                                step: Some(auth_step::Step::Session(Session {
                                    user_id,
                                    session_token: session_token.clone().into(),
                                })),
                            };

                            svc.deps.valid_sessions.insert(session_token, user_id);
                        }
                        "register" => {
                            if svc.deps.config.policy.disable_registration {
                                let token_raw = try_get_token(&mut values)?;
                                let token_hashed = hash_password(token_raw);
                                if svc
                                    .deps
                                    .auth_tree
                                    .get(&reg_token_key(token_hashed.as_ref()))?
                                    .is_none()
                                {
                                    return Err(ServerError::InvalidRegistrationToken.into());
                                }
                            }
                            let password_raw = try_get_password(&mut values)?;
                            let password_hashed = hash_password(password_raw);
                            let email = try_get_email(&mut values)?;
                            let username = try_get_username(&mut values)?;

                            if svc.deps.auth_tree.get(email.as_bytes())?.is_some() {
                                return Err(ServerError::UserAlreadyExists.into());
                            }

                            let user_id = gen_rand_u64();
                            let session_token = svc.gen_auth_token(); // [ref:alphanumeric_auth_token_gen] [ref:auth_token_length]

                            let mut batch = Batch::default();
                            batch.insert(email.into_bytes(), user_id.to_be_bytes().to_vec());
                            batch.insert(
                                user_id.to_be_bytes().to_vec(),
                                password_hashed.as_ref().to_vec(),
                            );
                            // [ref:token_u64_key]
                            batch.insert(
                                token_key(user_id).to_vec(),
                                session_token.as_str().as_bytes().to_vec(),
                            );
                            batch.insert(
                                // [ref:atime_u64_key]
                                atime_key(user_id).to_vec(),
                                // [ref:atime_u64_value]
                                get_time_secs().to_be_bytes().to_vec(),
                            );
                            svc.deps
                                .auth_tree
                                .inner
                                .apply_batch(batch)
                                .expect("failed to register into db");

                            let buf = rkyv_ser(&Profile {
                                user_name: username,
                                ..Default::default()
                            });
                            svc.deps
                                .profile_tree
                                .insert(make_user_profile_key(user_id), buf)?;

                            tracing::debug!("new user {} registered", user_id,);

                            next_step = AuthStep {
                                can_go_back: false,
                                fallback_url: String::default(),
                                step: Some(auth_step::Step::Session(Session {
                                    user_id,
                                    session_token: session_token.clone().into(),
                                })),
                            };

                            svc.deps.valid_sessions.insert(session_token, user_id);
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

    if let Some(chan) = svc.send_step.get(auth_id.as_str()) {
        tracing::debug!("sending next step to {} stream", auth_id);
        if let Err(err) = chan.send(next_step.clone()).await {
            tracing::error!("failed to send auth step to {}: {}", auth_id, err);
        }
    } else {
        tracing::debug!("no stream found for auth id {}, pushing to queue", auth_id);
        svc.queued_steps
            .entry(auth_id.clone())
            .and_modify(|s| s.push(next_step.clone()))
            .or_insert_with(|| vec![next_step.clone()]);
    }

    if let Some(auth_step::Step::Session(session)) = &next_step.step {
        tracing::debug!(
            "auth session {} complete with session {:#?}",
            auth_id,
            session
        );
        svc.step_map.remove(auth_id.as_str());
        svc.queued_steps.remove(auth_id.as_str());
    }

    Ok((NextStepResponse {
        step: Some(next_step),
    })
    .into_response())
}
