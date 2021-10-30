use super::*;

pub async fn handler(
    svc: &mut MediaproxyServer,
    request: Request<FetchLinkMetadataRequest>,
) -> ServerResult<Response<FetchLinkMetadataResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let FetchLinkMetadataRequest { url } = request.into_message().await?;

    let data = svc.fetch_metadata(url).await?.into();

    Ok((FetchLinkMetadataResponse { data: Some(data) }).into_response())
}
