use super::*;

pub async fn handler(
    _svc: &ChatServer,
    _request: Request<CreateDirectMessageRequest>,
) -> ServerResult<Response<CreateDirectMessageResponse>> {
    Err(ServerError::NotImplemented.into())
}
