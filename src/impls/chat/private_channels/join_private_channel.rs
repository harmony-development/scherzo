use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<JoinPrivateChannelRequest>,
) -> ServerResult<Response<JoinPrivateChannelResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let JoinPrivateChannelRequest { channel_id } = request.into_message().await?;

    let invite_id = channel_id.to_string();
    let channel_id = svc
        .deps
        .chat_tree
        .use_priv_invite_logic(user_id, &invite_id)
        .await?;

    logic(svc.deps.as_ref(), user_id, channel_id).await?;

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

pub async fn logic(deps: &Dependencies, user_id: u64, channel_id: u64) -> ServerResult<()> {
    let mut private_channel = get_private_channel::logic(deps, channel_id).await?;

    if private_channel.members.contains(&user_id) {
        bail!(("h.already-joined", "you are already in the private channel"));
    }

    let other_user_id = private_channel.members[0];
    private_channel.members.push(user_id);

    let mut batch = Batch::default();
    batch.insert(make_pc_key(channel_id), rkyv_ser(&private_channel));
    if private_channel.is_dm {
        batch.insert(
            make_dm_with_user_key(user_id, other_user_id),
            channel_id.to_be_bytes(),
        );
    }
    deps.chat_tree.apply_batch(batch).await?;

    Ok(())
}
