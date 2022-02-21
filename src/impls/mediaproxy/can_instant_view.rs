use super::*;

pub async fn handler(
    svc: &MediaproxyServer,
    request: Request<CanInstantViewRequest>,
) -> ServerResult<Response<CanInstantViewResponse>> {
    svc.deps.auth(&request).await?;

    let CanInstantViewRequest { url: urls } = request.into_message().await?;

    let mut can_instant_view = HashMap::with_capacity(urls.len());
    for url in urls {
        let metadata = svc.fetch_metadata(url.clone()).await?;
        let ok = matches!(metadata, Metadata::Site(_));
        can_instant_view.insert(url, ok);
    }

    Ok(CanInstantViewResponse::new(can_instant_view).into_response())
}
