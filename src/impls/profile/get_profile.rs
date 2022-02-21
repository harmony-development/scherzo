use super::*;

pub async fn handler(
    svc: &ProfileServer,
    request: Request<GetProfileRequest>,
) -> ServerResult<Response<GetProfileResponse>> {
    svc.deps.auth(&request).await?;

    let GetProfileRequest { user_id: user_ids } = request.into_message().await?;

    let mut profiles = HashMap::with_capacity(user_ids.len());
    for user_id in user_ids {
        let profile = svc.deps.profile_tree.get_profile_logic(user_id).await?;
        profiles.insert(user_id, profile);
    }

    Ok(GetProfileResponse::new(profiles).into_response())
}
