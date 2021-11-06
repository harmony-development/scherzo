use super::*;

pub async fn handler(
    svc: &AuthServer,
    request: Request<CheckLoggedInRequest>,
) -> Result<Response<CheckLoggedInResponse>, HrpcServerError> {
    svc.deps.valid_sessions.auth(&request)?;
    Ok((CheckLoggedInResponse {}).into_response())
}
