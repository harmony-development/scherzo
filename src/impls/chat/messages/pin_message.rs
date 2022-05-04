use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<PinMessageRequest>,
) -> ServerResult<Response<PinMessageResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let PinMessageRequest {
        guild_id,
        channel_id,
        message_id,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree
        .check_channel_user(guild_id, user_id, channel_id)
        .await?;

    chat_tree
        .check_perms(
            guild_id,
            Some(channel_id),
            user_id,
            all_permissions::MESSAGES_PINS_ADD,
            false,
        )
        .await?;

    let key = make_pinned_msgs_key(guild_id, channel_id);
    let mut pinned_msgs_raw = chat_tree.get(&key).await?.map_or_else(Vec::new, EVec::into);
    pinned_msgs_raw.extend_from_slice(&message_id.to_be_bytes());
    chat_tree.insert(key, pinned_msgs_raw).await?;

    svc.broadcast(
        guild_id.map_or(EventSub::PrivateChannel(channel_id), EventSub::Guild),
        stream_event::Event::MessagePinned(stream_event::MessagePinned {
            guild_id,
            channel_id,
            message_id,
        }),
        PermCheck::maybe_new(guild_id, channel_id, all_permissions::MESSAGES_VIEW),
        EventContext::empty(),
    );

    Ok(PinMessageResponse::new().into_response())
}
