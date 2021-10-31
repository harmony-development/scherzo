use super::*;

pub async fn handler(
    _svc: &mut ChatServer,
    _request: Request<CreateRoomRequest>,
) -> ServerResult<Response<CreateRoomResponse>> {
    Err(ServerError::NotImplemented.into())
}
