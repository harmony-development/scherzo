use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetPrivateChannelListRequest>,
) -> ServerResult<Response<GetPrivateChannelListResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let channels = svc
        .deps
        .chat_tree
        .get_user_private_channels(user_id)
        .await?;

    Ok(GetPrivateChannelListResponse::new(channels).into_response())
}
