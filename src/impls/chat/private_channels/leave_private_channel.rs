use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<LeavePrivateChannelRequest>,
) -> ServerResult<Response<LeavePrivateChannelResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let LeavePrivateChannelRequest { channel_id } = request.into_message().await?;

    svc.deps
        .chat_tree
        .check_private_channel_user(channel_id, user_id)
        .await?;

    logic(svc.deps.as_ref(), user_id, channel_id).await?;

    svc.broadcast(
        EventSub::PrivateChannel(channel_id),
        stream_event::Event::UserLeftPrivateChannel(stream_event::UserLeftPrivateChannel {
            channel_id,
            user_id,
        }),
        None,
        EventContext::empty(),
    );

    svc.dispatch_private_channel_leave(channel_id, user_id)
        .await?;

    Ok(LeavePrivateChannelResponse::new().into_response())
}

pub async fn logic(deps: &Dependencies, user_id: u64, channel_id: u64) -> ServerResult<()> {
    if deps
        .chat_tree
        .is_user_private_channel_creator(channel_id, user_id)
        .await?
    {
        bail!((
            "h.cant-leave",
            "private channel creators cant leave the channel, they must delete it"
        ));
    }

    let mut private_channel = get_private_channel::logic(deps, channel_id).await?;

    if let Some(pos) = private_channel.members.iter().position(|id| user_id.eq(id)) {
        private_channel.members.remove(pos);
    } else {
        bail!(ServerError::UserNotInPrivateChannel {
            channel_id,
            user_id
        });
    }

    deps.chat_tree
        .insert(make_pc_key(channel_id), rkyv_ser(&private_channel))
        .await?;

    Ok(())
}
