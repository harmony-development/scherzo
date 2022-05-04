use super::*;

pub async fn handler(
    svc: &AuthServer,
    request: Request<CheckLoggedInRequest>,
) -> Result<Response<CheckLoggedInResponse>, HrpcServerError> {
    svc.deps.auth(&request).await?;
    Ok(CheckLoggedInResponse::new().into_response())
}
