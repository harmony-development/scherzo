use super::*;

pub async fn handle(svc: &AuthServer, values: &mut Vec<Field>) -> ServerResult<AuthStep> {
    let auth_tree = &svc.deps.auth_tree;

    if svc.deps.config.policy.disable_registration {
        let token_raw = try_get_token(values)?;
        auth_tree.validate_single_use_token(token_raw).await?;
    }

    let password_raw = try_get_password(values)?;
    if password_raw.is_empty() {
        bail!(("h.invalid-password", "password can't be empty"));
    }

    let password_hashed = hash_password(password_raw);
    let email = try_get_email(values)?;
    let username = try_get_username(values)?;

    if username.is_empty() {
        bail!(("h.invalid-username", "username can't be empty"));
    }

    if auth_tree.get(email.as_bytes()).await?.is_some() {
        bail!(ServerError::UserAlreadyExists);
    }

    let user_id = svc.gen_user_id().await?;
    let session_token = svc.gen_auth_token(); // [ref:alphanumeric_auth_token_gen] [ref:auth_token_length]

    let mut batch = Batch::default();
    batch.insert(email.into_bytes(), user_id.to_be_bytes());
    batch.insert(user_id.to_be_bytes(), password_hashed.as_ref());
    // [ref:token_u64_key]
    batch.insert(token_key(user_id), session_token.as_str().as_bytes());
    batch.insert(
        // [ref:atime_u64_key]
        atime_key(user_id),
        // [ref:atime_u64_value]
        get_time_secs().to_be_bytes(),
    );
    auth_tree.apply_batch(batch).await?;

    let buf = rkyv_ser(&Profile {
        user_name: username,
        ..Default::default()
    });
    svc.deps
        .profile_tree
        .insert(make_user_profile_key(user_id), buf)
        .await?;

    tracing::debug!("new user {} registered", user_id);

    svc.deps
        .valid_sessions
        .insert(session_token.clone(), user_id);

    Ok(AuthStep {
        can_go_back: false,
        fallback_url: String::default(),
        step: Some(auth_step::Step::Session(Session {
            user_id,
            session_token: session_token.into(),
        })),
    })
}
