use super::*;

pub async fn handler(
    _svc: &SyncServer,
    _request: Request<NotifyNewIdRequest>,
) -> ServerResult<Response<NotifyNewIdResponse>> {
    Err(ServerError::NotImplemented.into())
}
