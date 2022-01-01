use super::*;

pub async fn handler(
    svc: &AuthServer,
    request: Request<CheckLoggedInRequest>,
) -> Result<Response<CheckLoggedInResponse>, HrpcServerError> {
    svc.deps.auth(&request).await?;
    Ok((CheckLoggedInResponse {}).into_response())
}
