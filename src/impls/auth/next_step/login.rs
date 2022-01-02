use super::*;

pub async fn handle(svc: &AuthServer, values: &mut Vec<Field>) -> ServerResult<AuthStep> {
    let auth_tree = &svc.deps.auth_tree;

    let password_raw = try_get_password(values)?;
    let email = try_get_email(values)?;

    let user_id = auth_tree.get_user_id(&email).await?;

    // check password
    let is_password_correct =
        auth_tree
            .get(user_id.to_be_bytes().as_ref())
            .await?
            .map_or(false, |pass_hash| {
                let pass_hash = unsafe { std::str::from_utf8_unchecked(pass_hash.as_ref()) };
                verify_password(password_raw, pass_hash)
            });
    if !is_password_correct {
        bail!(ServerError::WrongEmailOrPassword {
            email: email.into(),
        });
    }

    let session_token = svc.gen_auth_token().await?; // [ref:alphanumeric_auth_token_gen] [ref:auth_token_length]
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

    auth_tree
        .insert(auth_key(session_token.as_str()), user_id.to_be_bytes())
        .await?;

    Ok(AuthStep {
        can_go_back: false,
        fallback_url: String::default(),
        step: Some(auth_step::Step::Session(Session {
            user_id,
            session_token: session_token.into(),
        })),
    })
}
