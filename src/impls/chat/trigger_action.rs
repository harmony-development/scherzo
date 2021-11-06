use super::*;

pub async fn handler(
    _svc: &ChatServer,
    _request: Request<TriggerActionRequest>,
) -> ServerResult<Response<TriggerActionResponse>> {
    Err(ServerError::NotImplemented.into())
}
