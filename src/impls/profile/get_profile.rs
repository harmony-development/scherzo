use super::*;

pub async fn handler(
    svc: &mut ProfileServer,
    request: Request<GetProfileRequest>,
) -> ServerResult<Response<GetProfileResponse>> {
    svc.deps.valid_sessions.auth(&request)?;

    let GetProfileRequest { user_id } = request.into_message().await?;

    svc.deps
        .profile_tree
        .get_profile_logic(user_id)
        .map(|p| GetProfileResponse { profile: Some(p) })
        .map(Response::new)
        .map_err(Into::into)
}
