use super::*;

pub async fn handler(
    svc: &BatchServer,
    mut request: Request<BatchRequest>,
) -> ServerResult<Response<BatchResponse>> {
    let auth_header = request
        .header_map_mut()
        .and_then(|h| h.remove(header::AUTHORIZATION));
    let BatchRequest { requests } = request.into_message().await?;

    let request_len = requests.len();
    let (bodies, endpoints) = requests.into_iter().fold(
        (
            Vec::with_capacity(request_len),
            Vec::with_capacity(request_len),
        ),
        |(mut bodies, mut endpoints), request| {
            bodies.push(request.request);
            endpoints.push(request.endpoint);
            (bodies, endpoints)
        },
    );
    let responses = svc
        .make_req(bodies, Endpoint::Different(endpoints), auth_header)
        .await?;

    Ok((BatchResponse { responses }).into_response())
}
