use super::*;

pub async fn handler(
    _svc: &mut SyncServer,
    _request: Request<NotifyNewIdRequest>,
) -> ServerResult<Response<NotifyNewIdResponse>> {
    Err(ServerError::NotImplemented.into())
}
