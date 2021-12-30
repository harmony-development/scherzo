use crate::impls::send_email;

use super::*;

const EMAIL_BODY_TEMPLATE: &str = include_str!("email_body_template.txt");

// "delete-user-input-token"
pub async fn handle_input_token(
    svc: &AuthServer,
    values: &mut Vec<Field>,
) -> ServerResult<AuthStep> {
    let token = try_get_token(values)?;

    let raw_user_id = svc.deps.auth_tree.validate_single_use_token(token).await?;
    let user_id = db::deser_id(raw_user_id);

    crate::impls::auth::delete_user::logic(svc.deps.as_ref(), user_id).await?;

    Ok(back_to_inital_step())
}

// "delete-user-send-token"
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
    let body = EMAIL_BODY_TEMPLATE
        .replace("{action}", "deleting your account")
        .replace("{token}", token.as_str());
    let subject = format!("Harmony - Account Deletion for {}", &svc.deps.config.host);
    send_email(svc.deps.as_ref(), &user_email, subject, body).await?;

    Ok(AuthStep {
        can_go_back: false,
        fallback_url: String::default(),
        step: Some(auth_step::Step::new_form(auth_step::Form::new(
            "delete-user-input-token".to_string(),
            vec![auth_step::form::FormField::new(
                "token".to_string(),
                "password".to_string(),
            )],
        ))),
    })
}
