use super::*;

pub async fn handler(
    svc: &ProfileServer,
    request: Request<GetAppDataRequest>,
) -> ServerResult<Response<GetAppDataResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let GetAppDataRequest { app_id } = request.into_message().await?;
    let app_data = svc
        .deps
        .profile_tree
        .get(make_user_metadata_key(user_id, &app_id))
        .await?
        .unwrap_or_default()
        .into();

    Ok((GetAppDataResponse { app_data }).into_response())
}
