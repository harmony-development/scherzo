use super::*;

pub async fn handler(
    _svc: &mut ChatServer,
    _request: Request<GetPinnedMessagesRequest>,
) -> ServerResult<Response<GetPinnedMessagesResponse>> {
    Err(ServerError::NotImplemented.into())
}