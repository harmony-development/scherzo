use super::*;

pub async fn handler(
    svc: &ProfileServer,
    request: Request<SetAppDataRequest>,
) -> ServerResult<Response<SetAppDataResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let SetAppDataRequest { app_id, app_data } = request.into_message().await?;
    svc.deps
        .profile_tree
        .insert(make_user_metadata_key(user_id, &app_id), app_data)
        .await?;

    Ok((SetAppDataResponse {}).into_response())
}
