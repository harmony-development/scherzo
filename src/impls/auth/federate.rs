use super::*;

pub async fn handler(
    svc: &mut AuthServer,
    request: Request<FederateRequest>,
) -> Result<Response<FederateResponse>, HrpcServerError> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let keys_manager = svc.keys_manager()?;

    let profile = svc.profile_tree.get_profile_logic(user_id)?;
    let server_id = request.into_message().await?.server_id;

    svc.is_host_allowed(&server_id)?;

    let data = TokenData {
        user_id,
        server_id,
        username: profile.user_name,
        avatar: profile.user_avatar,
    };

    let token = keys_manager.generate_token(data).await?;

    Ok((FederateResponse { token: Some(token) }).into_response())
}
