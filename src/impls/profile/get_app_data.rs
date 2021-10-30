use super::*;

pub async fn handler(
    svc: &mut ProfileServer,
    request: Request<GetAppDataRequest>,
) -> ServerResult<Response<GetAppDataResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let GetAppDataRequest { app_id } = request.into_message().await?;
    let app_data = svc
        .profile_tree
        .get(make_user_metadata_key(user_id, &app_id))?
        .unwrap_or_default();

    Ok((GetAppDataResponse { app_data }).into_response())
}
