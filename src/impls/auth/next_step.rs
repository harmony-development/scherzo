use harmony_rust_sdk::api::auth::{auth_step::form::FormField, next_step_request::FormFields};

use super::*;

pub mod delete_user;
pub mod login;
pub mod registration;
pub mod reset_password;

// While implementing new choices / forms, make sure to:
// - handle the choice / from in `handle_choice` or `handle_fields` respectively
// - handle the form title in this handler
pub async fn handler(
    svc: &AuthServer,
    req: Request<NextStepRequest>,
) -> ServerResult<Response<NextStepResponse>> {
    let NextStepRequest {
        auth_id,
        step: maybe_step,
    } = req.into_message().await?;

    let auth_id: SmolStr = auth_id.into();

    let fut = async {
        // get step stack for this auth id (the stack is initialized in begin_auth)
        let Some(mut step_stack) = svc.step_map.get_mut(auth_id.as_str()) else {
        bail!(ServerError::InvalidAuthId);
    };

        // get the next step if possible
        let next_step;
        match maybe_step {
            None => {
                // Safety: step stack can never be empty [ref:step_stack_non_empty]
                next_step = unsafe { step_stack.last().unwrap_unchecked().clone() };
            }
            Some(step) => {
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
                tracing::debug!("client replied with step");
                match step {
                    next_step_request::Step::Choice(next_step_request::Choice { choice }) => {
                        let auth_step::Step::Choice(auth_step::Choice { options, .. }) = current_step else {
                            bail!(ServerError::WrongStep {
                                expected: SmolStr::new_inline("form"),
                                got: SmolStr::new_inline("choice"),
                            });
                        };

                        tracing::debug!("handling choices: {:?}", options);
                        tracing::debug!("user chose {}", choice);

                        if options.contains(&choice) {
                            next_step = handle_choice(svc, choice.as_str())?;
                            step_stack.push(next_step.clone());
                        } else {
                            bail!(ServerError::NoSuchChoice {
                                choice: choice.into(),
                                expected_any_of: options.into_iter().map(Into::into).collect(),
                            });
                        }
                    }
                    next_step_request::Step::Form(next_step_request::Form { fields }) => {
                        let auth_step::Step::Form(auth_step::Form {
                            fields: auth_fields,
                            title,
                        }) = current_step else {
                            bail!(ServerError::WrongStep {
                                expected: SmolStr::new_inline("choice"),
                                got: SmolStr::new_inline("form"),
                            });
                        };

                        tracing::debug!("handling form '{}'", title);

                        let mut values = Vec::with_capacity(fields.len());

                        handle_fields(svc, &mut values, fields, auth_fields)?;

                        // handle new forms here
                        next_step = match title.as_str() {
                            "login" => login::handle(svc, &mut values).await?,
                            "register" => registration::handle(svc, &mut values).await?,
                            "register-input-token" => {
                                registration::handle_input_token(svc, &mut values).await?
                            }
                            "delete-user-input-token" => {
                                delete_user::handle_input_token(svc, &mut values).await?
                            }
                            "delete-user-send-token" => {
                                delete_user::handle_send_token(svc, &mut values).await?
                            }
                            "reset-password-input-token" => {
                                reset_password::handle_input_token(svc, &mut values).await?
                            }
                            "reset-password-send-token" => {
                                reset_password::handle_send_token(svc, &mut values).await?
                            }
                            title => bail!((
                                "h.invalid-form",
                                format!("invalid form name used: {}", title)
                            )),
                        };
                    }
                }
            }
        }

        drop(step_stack);

        if let Some(chan) = svc.send_step.get(auth_id.as_str()) {
            tracing::debug!("sending next step: {:?}", next_step);
            if let Err(err) = chan.send(next_step.clone()).await {
                tracing::error!("failed to send auth step: {}", err);
            }
        } else {
            tracing::debug!("no stream found, pushing to queue");
            svc.queued_steps
                .entry(auth_id.clone())
                .and_modify(|s| s.push(next_step.clone()))
                .or_insert_with(|| vec![next_step.clone()]);
        }

        if matches!(next_step.step, Some(auth_step::Step::Session(_))) {
            tracing::debug!("auth session complete");
            svc.step_map.remove(auth_id.as_str());
            svc.queued_steps.remove(auth_id.as_str());
        }

        Ok((NextStepResponse {
            step: Some(next_step),
        })
        .into_response())
    };

    fut.instrument(tracing::debug_span!("next_step", auth_id = %auth_id))
        .await
}

fn form<'a>(
    title: impl std::fmt::Display,
    steps: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<auth_step::Step> {
    Some(auth_step::Step::Form(auth_step::Form::new(
        title.to_string(),
        steps
            .into_iter()
            .map(|(name, r#type)| {
                auth_step::form::FormField::new(name.to_string(), r#type.to_string())
            })
            .collect(),
    )))
}

pub fn handle_choice(svc: &AuthServer, choice: &str) -> ServerResult<AuthStep> {
    let step = match choice {
        "back-to-initial" => initial_auth_step(),
        "other-options" => {
            let mut options = Vec::new();

            if svc.deps.config.email.is_some() {
                options.extend(
                    ["delete-user", "reset-password"]
                        .iter()
                        .map(ToString::to_string),
                );
            }

            AuthStep {
                can_go_back: true,
                fallback_url: String::default(),
                step: Some(auth_step::Step::new_choice(auth_step::Choice::new(
                    "other-options".to_string(),
                    options,
                ))),
            }
        }
        "reset-password" => AuthStep {
            can_go_back: true,
            fallback_url: String::default(),
            step: form("reset-password-send-token", [("email", "email")]),
        },
        "delete-user" => AuthStep {
            can_go_back: true,
            fallback_url: String::default(),
            step: form("delete-user-send-token", [("email", "email")]),
        },
        "login" => AuthStep {
            can_go_back: true,
            fallback_url: String::default(),
            step: form("login", [("email", "email"), ("password", "password")]),
        },
        "register" => {
            let config = &svc.deps.config;
            let mut fields = vec![
                ("email", "email"),
                ("username", "text"),
                ("password", "password"),
            ];
            if config.policy.disable_registration {
                fields.push(("token", "password"));
            }
            AuthStep {
                can_go_back: true,
                fallback_url: String::default(),
                step: form("register", fields),
            }
        }
        choice => bail!((
            "h.invalid-choice",
            format!("got invalid choice: {}", choice),
        )),
    };

    Ok(step)
}

pub fn handle_fields(
    _svc: &AuthServer,
    values: &mut Vec<Field>,
    fields: Vec<FormFields>,
    auth_fields: Vec<FormField>,
) -> ServerResult<()> {
    for (index, field) in fields.into_iter().enumerate() {
        let Some(afield) = auth_fields.get(index) else {
            bail!(ServerError::NoSuchField);
        };
        let Some(field) = field.field else {
            bail!(ServerError::NoFieldSpecified);
        };

        tracing::debug!("handling field {} (type {})", afield.name, afield.r#type);
        let reply_field_type = match field {
            Field::Bytes(_) => "bytes",
            Field::String(_) => "string",
            Field::Number(_) => "number",
        };
        tracing::debug!("reply field is type {}", reply_field_type);

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
            field => bail!((
                "h.invalid-field-type",
                format!("got invalid field type: {}", field)
            )),
        }
    }

    Ok(())
}
