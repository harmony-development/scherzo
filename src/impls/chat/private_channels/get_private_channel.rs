use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetPrivateChannelRequest>,
) -> ServerResult<Response<GetPrivateChannelResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let GetPrivateChannelRequest { channel_ids } = request.into_message().await?;

    for channel_id in channel_ids.iter().copied() {
        svc.deps
            .chat_tree
            .check_private_channel_user(channel_id, user_id)
            .await?;
    }

    let mut channels = HashMap::with_capacity(channel_ids.len());
    for channel_id in channel_ids {
        let channel = logic(svc.deps.as_ref(), channel_id).await?;
        channels.insert(channel_id, channel);
    }

    Ok(GetPrivateChannelResponse::new(channels).into_response())
}

pub async fn logic(deps: &Dependencies, channel_id: u64) -> ServerResult<PrivateChannel> {
    deps.chat_tree
        .get(make_pc_key(channel_id))
        .await?
        .ok_or(ServerError::NoSuchPrivateChannel(channel_id))
        .map(db::deser_private_channel)
        .map_err(Into::into)
}
