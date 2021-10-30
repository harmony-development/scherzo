use super::*;

pub async fn handler(
    svc: &mut AuthServer,
    request: Request<CheckLoggedInRequest>,
) -> Result<Response<CheckLoggedInResponse>, HrpcServerError> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;
    Ok(((CheckLoggedInResponse {}).into_response()).into_response())
}
