use super::*;

pub async fn handler(
    _svc: &ChatServer,
    _request: Request<v1::UpgradeRoomToGuildRequest>,
) -> ServerResult<Response<v1::UpgradeRoomToGuildResponse>> {
    Err(ServerError::NotImplemented.into())
}
