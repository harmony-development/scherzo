use super::*;

pub async fn handler(
    _svc: &ChatServer,
    _request: Request<v1::GetPendingInvitesRequest>,
) -> ServerResult<Response<v1::GetPendingInvitesResponse>> {
    Err(ServerError::NotImplemented.into())
}
