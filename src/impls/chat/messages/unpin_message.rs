use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<UnpinMessageRequest>,
) -> ServerResult<Response<UnpinMessageResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let UnpinMessageRequest {
        guild_id,
        channel_id,
        message_id,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree
        .check_guild_user_channel(guild_id, user_id, channel_id)
        .await?;

    chat_tree
        .check_perms(
            guild_id,
            Some(channel_id),
            user_id,
            all_permissions::MESSAGES_PINS_REMOVE,
            false,
        )
        .await?;

    let mut pinned_message_ids = chat_tree
        .get_pinned_messages_logic(guild_id, channel_id)
        .await?;
    if let Some(pos) = pinned_message_ids.iter().position(|id| message_id.eq(id)) {
        pinned_message_ids.remove(pos);
    }
    let pinned_msgs_raw = chat_tree.serialize_list_u64_logic(pinned_message_ids);
    chat_tree
        .insert(make_pinned_msgs_key(guild_id, channel_id), pinned_msgs_raw)
        .await?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::MessageUnpinned(stream_event::MessageUnpinned {
            guild_id,
            channel_id,
            message_id,
        }),
        Some(PermCheck::new(
            guild_id,
            Some(channel_id),
            all_permissions::MESSAGES_VIEW,
            false,
        )),
        EventContext::empty(),
    );

    Ok(UnpinMessageResponse::new().into_response())
}
