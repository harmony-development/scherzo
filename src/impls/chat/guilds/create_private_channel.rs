use super::*;

pub async fn handler(
    _svc: &ChatServer,
    _request: Request<CreatePrivateChannelRequest>,
) -> ServerResult<Response<CreatePrivateChannelResponse>> {
    Err(ServerError::NotImplemented.into())
}
