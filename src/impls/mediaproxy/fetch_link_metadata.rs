use super::*;

pub async fn handler(
    svc: &MediaproxyServer,
    request: Request<FetchLinkMetadataRequest>,
) -> ServerResult<Response<FetchLinkMetadataResponse>> {
    svc.deps.auth(&request).await?;

    let FetchLinkMetadataRequest { url: urls } = request.into_message().await?;

    let mut data = HashMap::with_capacity(urls.len());
    for url in urls {
        let metadata = svc.fetch_metadata(url.clone()).await?.into();
        data.insert(url, metadata);
    }

    // TODO: return fetch errors
    Ok(FetchLinkMetadataResponse::new(data, HashMap::new()).into_response())
}
