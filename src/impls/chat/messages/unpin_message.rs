use super::*;

pub async fn handler(
    _svc: &ChatServer,
    _request: Request<UnpinMessageRequest>,
) -> ServerResult<Response<UnpinMessageResponse>> {
    Err(ServerError::NotImplemented.into())
}
