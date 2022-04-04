use super::*;

pub async fn handler(
    _svc: &ChatServer,
    _request: Request<UpdatePrivateChannelMembersRequest>,
) -> ServerResult<Response<UpdatePrivateChannelMembersResponse>> {
    Err(ServerError::NotImplemented.into())
}
