use super::*;

pub async fn handler(
    svc: &BatchServer,
    mut request: Request<BatchSameRequest>,
) -> ServerResult<Response<BatchSameResponse>> {
    let auth_header = request.header_map_mut().remove(&header::AUTHORIZATION);
    let BatchSameRequest { endpoint, requests } = request.into_message().await?;

    let responses = svc
        .make_req(requests, Endpoint::Same(endpoint), auth_header)
        .await?;

    Ok((BatchSameResponse { responses }).into_response())
}
