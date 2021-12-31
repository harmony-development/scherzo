use rkyv::{de::deserializers::SharedDeserializeMap, Archive, Deserialize, Serialize};

use crate::impls::send_email;

use super::*;

const EMAIL_BODY_TEMPLATE: &str = include_str!("email_body_template.txt");

#[derive(Debug, Archive, Serialize, Deserialize)]
struct RegInfo {
    email: String,
    username: String,
    password_raw: Vec<u8>,
}

pub async fn handle(svc: &AuthServer, values: &mut Vec<Field>) -> ServerResult<AuthStep> {
    let auth_tree = &svc.deps.auth_tree;
    let config = &svc.deps.config;

    if config.policy.disable_registration {
        let token_raw = try_get_token(values)?;
        auth_tree.validate_single_use_token(token_raw).await?;
    }

    let password_raw = try_get_password(values)?;
    let username = try_get_username(values)?;
    let email = try_get_email(values)?;

    if svc.deps.email.is_some()
        && config.email.is_some()
        && !config.policy.disable_registration_email_validation
    {
        let reg_info = RegInfo {
            email,
            username,
            password_raw,
        };
        let reg_info_serialized = rkyv_ser(&reg_info);
        let token = auth_tree
            .generate_single_use_token(reg_info_serialized)
            .await?;
        let body = EMAIL_BODY_TEMPLATE
            .replace("{action}", "registering")
            .replace("{token}", token.as_str());
        let subject = format!("Harmony - Register for {}", &svc.deps.config.host);
        send_email(svc.deps.as_ref(), &reg_info.email, subject, body).await?;

        return Ok(AuthStep {
            can_go_back: false,
            fallback_url: String::default(),
            step: form("register-input-token", [("token", "password")]),
        });
    }

    logic(svc, password_raw, username, email).await
}

pub async fn handle_input_token(
    svc: &AuthServer,
    values: &mut Vec<Field>,
) -> ServerResult<AuthStep> {
    let token = try_get_token(values)?;

    let reg_info_raw = svc.deps.auth_tree.validate_single_use_token(token).await?;
    let reg_info: RegInfo = rkyv_arch::<RegInfo>(&reg_info_raw)
        .deserialize(&mut SharedDeserializeMap::default())
        .expect("must be correct");

    logic(
        svc,
        reg_info.password_raw,
        reg_info.username,
        reg_info.email,
    )
    .await
}

pub async fn logic(
    svc: &AuthServer,
    password_raw: Vec<u8>,
    username: String,
    email: String,
) -> ServerResult<AuthStep> {
    let auth_tree = &svc.deps.auth_tree;

    if password_raw.is_empty() {
        bail!(("h.invalid-password", "password can't be empty"));
    }
    let password_hashed = hash_password(password_raw);

    if username.is_empty() {
        bail!(("h.invalid-username", "username can't be empty"));
    }

    if email.is_empty() {
        bail!(("h.invalid-email", "email can't be empty"));
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
