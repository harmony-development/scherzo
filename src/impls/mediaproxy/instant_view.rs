use super::*;

pub async fn handler(
    svc: &MediaproxyServer,
    request: Request<InstantViewRequest>,
) -> ServerResult<Response<InstantViewResponse>> {
    svc.deps.auth(&request).await?;

    let InstantViewRequest { url } = request.into_message().await?;

    let data = svc.fetch_metadata(url).await?;

    let msg = if let Metadata::Site(html) = data {
        let metadata = site_metadata_from_html(&html);

        InstantViewResponse {
            content: html.text_content.clone(),
            is_valid: true,
            metadata: Some(metadata),
        }
    } else {
        InstantViewResponse::default().with_is_valid(false)
    };

    Ok(msg.into_response())
}
