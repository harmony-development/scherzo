use super::*;

pub async fn handler(
    svc: &mut MediaproxyServer,
    request: Request<CanInstantViewRequest>,
) -> ServerResult<Response<CanInstantViewResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let CanInstantViewRequest { url } = request.into_message().await?;

    if let Some(val) = get_from_cache(&url) {
        return Ok((CanInstantViewResponse {
            can_instant_view: matches!(val.value, Metadata::Site(_)),
        })
        .into_response());
    }

    let url: Uri = url.parse().map_err(ServerError::InvalidUrl)?;
    let response = svc.http.get(url).await.map_err(ServerError::from)?;

    let ok = get_mimetype(&response).eq("text/html");

    Ok((CanInstantViewResponse {
        can_instant_view: ok,
    })
    .into_response())
}
