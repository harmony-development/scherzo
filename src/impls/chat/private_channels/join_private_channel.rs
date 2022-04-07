use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<JoinPrivateChannelRequest>,
) -> ServerResult<Response<JoinPrivateChannelResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let JoinPrivateChannelRequest { channel_id } = request.into_message().await?;

    let channel_id = logic(svc.deps.as_ref(), user_id, channel_id).await?;

    svc.broadcast(
        EventSub::PrivateChannel(channel_id),
        stream_event::Event::UserJoinedPrivateChannel(stream_event::UserJoinedPrivateChannel {
            channel_id,
            user_id,
        }),
        None,
        EventContext::empty(),
    );

    svc.dispatch_private_channel_join(channel_id, user_id)
        .await?;

    Ok(JoinPrivateChannelResponse::new().into_response())
}

pub async fn logic(deps: &Dependencies, user_id: u64, channel_id: u64) -> ServerResult<u64> {
    let invite_id = channel_id.to_string();
    let channel_id = deps
        .chat_tree
        .use_priv_invite_logic(user_id, &invite_id)
        .await?;

    let mut private_channel = deps.chat_tree.get_private_channel_logic(channel_id).await?;

    if private_channel.members.contains(&user_id) {
        bail!(("h.already-joined", "you are already in the private channel"));
    }

    private_channel.members.push(user_id);

    let serialized = rkyv_ser(&private_channel);
    deps.chat_tree
        .insert(make_pc_key(channel_id), serialized)
        .await?;

    Ok(channel_id)
}
