use super::*;

pub async fn handler(
    svc: &ProfileServer,
    request: Request<GetProfileRequest>,
) -> ServerResult<Response<GetProfileResponse>> {
    svc.deps.auth(&request).await?;

    let GetProfileRequest { user_id } = request.into_message().await?;

    svc.deps
        .profile_tree
        .get_profile_logic(user_id)
        .await
        .map(|p| GetProfileResponse { profile: Some(p) })
        .map(IntoResponse::into_response)
        .map_err(Into::into)
}
