use super::*;

pub async fn handler(
    svc: &MediaproxyServer,
    request: Request<CanInstantViewRequest>,
) -> ServerResult<Response<CanInstantViewResponse>> {
    svc.deps.auth(&request).await?;

    let CanInstantViewRequest { url } = request.into_message().await?;

    if let Some(val) = get_from_cache(&url) {
        return Ok((CanInstantViewResponse {
            can_instant_view: matches!(val.value, Metadata::Site(_)),
        })
        .into_response());
    }

    let response = svc
        .http
        .get(url)
        .send()
        .await
        .map_err(ServerError::FailedToFetchLink)?
        .error_for_status()
        .map_err(ServerError::FailedToFetchLink)?;

    let ok = get_mimetype(response.headers()).eq("text/html");

    Ok((CanInstantViewResponse {
        can_instant_view: ok,
    })
    .into_response())
}
