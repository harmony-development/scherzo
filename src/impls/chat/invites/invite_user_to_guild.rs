use super::*;

pub async fn handler(
    _svc: &ChatServer,
    _request: Request<v1::InviteUserToGuildRequest>,
) -> ServerResult<Response<v1::InviteUserToGuildResponse>> {
    Err(ServerError::NotImplemented.into())
}
