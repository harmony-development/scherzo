use super::*;

pub async fn handler(
    svc: &MediaproxyServer,
    request: Request<FetchLinkMetadataRequest>,
) -> ServerResult<Response<FetchLinkMetadataResponse>> {
    svc.deps.auth(&request).await?;

    let FetchLinkMetadataRequest { url } = request.into_message().await?;

    let data = svc.fetch_metadata(url).await?.into();

    Ok((FetchLinkMetadataResponse { data: Some(data) }).into_response())
}
