use super::*;

pub async fn handler(
    svc: &MediaproxyServer,
    request: Request<FetchLinkMetadataRequest>,
) -> ServerResult<Response<FetchLinkMetadataResponse>> {
    svc.deps.valid_sessions.auth(&request)?;

    let FetchLinkMetadataRequest { url } = request.into_message().await?;

    let data = svc.fetch_metadata(url).await?.into();

    Ok((FetchLinkMetadataResponse { data: Some(data) }).into_response())
}
