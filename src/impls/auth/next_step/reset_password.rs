use super::*;

// "reset-password-input-token"
pub async fn handle_input_token(
    svc: &AuthServer,
    values: &mut Vec<Field>,
) -> ServerResult<AuthStep> {
    let new_password_raw = try_get_password(values)?;
    let token = try_get_token(values)?;

    let raw_user_id = svc.deps.auth_tree.validate_single_use_token(token).await?;

    let hashed_password = hash_password(new_password_raw);
    svc.deps
        .auth_tree
        .insert(raw_user_id, hashed_password)
        .await?;

    Ok(back_to_inital_step())
}

// "reset-password-send-token"
pub async fn handle_send_token(
    svc: &AuthServer,
    values: &mut Vec<Field>,
) -> ServerResult<AuthStep> {
    let auth_tree = &svc.deps.auth_tree;

    let user_email = try_get_email(values)?;

    let user_id = auth_tree.get_user_id(&user_email).await?;

    let token = auth_tree
        .generate_single_use_token(user_id.to_be_bytes())
        .await?;

    email::send_token_email(
        svc.deps.as_ref(),
        &user_email,
        token.as_ref(),
        "reset password",
    )
    .await?;

    Ok(AuthStep {
        can_go_back: false,
        fallback_url: String::default(),
        step: form(
            "reset-password-input-token",
            [("token", "password"), ("new-password", "password")],
        ),
    })
}
