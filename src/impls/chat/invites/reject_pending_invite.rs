use super::*;

pub async fn handler(
    _svc: &mut ChatServer,
    _request: Request<v1::RejectPendingInviteRequest>,
) -> ServerResult<Response<v1::RejectPendingInviteResponse>> {
    Err(ServerError::NotImplemented.into())
}
