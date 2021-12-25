use super::*;

pub async fn handle(svc: &AuthServer, values: &mut Vec<Field>) -> ServerResult<AuthStep> {
    let auth_tree = &svc.deps.auth_tree;

    let password_raw = try_get_password(values)?;
    let password_hashed = hash_password(password_raw);
    let email = try_get_email(values)?;

    let maybe_user_id = auth_tree.get(email.as_bytes()).await?.map(|raw| {
        // Safety: this unwrap can never cause UB since we only store u64
        u64::from_be_bytes(unsafe { raw.try_into().unwrap_unchecked() })
    });
    let Some(user_id) = maybe_user_id else {
        bail!(ServerError::WrongEmailOrPassword {
            email: email.into(),
        });
    };

    // check password
    let is_password_correct = auth_tree
        .get(user_id.to_be_bytes().as_ref())
        .await?
        .map_or(false, |pass| pass.as_ref() == password_hashed.as_ref());
    if !is_password_correct {
        bail!(ServerError::WrongEmailOrPassword {
            email: email.into(),
        });
    }

    let session_token = svc.gen_auth_token(); // [ref:alphanumeric_auth_token_gen] [ref:auth_token_length]
    let mut batch = Batch::default();
    // [ref:token_u64_key]
    batch.insert(token_key(user_id), session_token.as_str().as_bytes());
    batch.insert(
        // [ref:atime_u64_key]
        atime_key(user_id),
        // [ref:atime_u64_value]
        get_time_secs().to_be_bytes(),
    );
    auth_tree.apply_batch(batch).await?;

    tracing::debug!("user {} logged in with email {}", user_id, email);

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
