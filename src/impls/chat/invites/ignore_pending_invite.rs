use super::*;

pub async fn handler(
    _svc: &ChatServer,
    _request: Request<v1::IgnorePendingInviteRequest>,
) -> ServerResult<Response<v1::IgnorePendingInviteResponse>> {
    Err(ServerError::NotImplemented.into())
}
