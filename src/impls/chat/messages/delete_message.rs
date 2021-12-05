use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<DeleteMessageRequest>,
) -> ServerResult<Response<DeleteMessageResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let DeleteMessageRequest {
        guild_id,
        channel_id,
        message_id,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree
        .check_guild_user_channel(guild_id, user_id, channel_id)
        .await?;
    if chat_tree
        .get_message_logic(guild_id, channel_id, message_id)
        .await?
        .0
        .author_id
        != user_id
    {
        chat_tree
            .check_perms(
                guild_id,
                Some(channel_id),
                user_id,
                "messages.manage.delete",
                false,
            )
            .await?;
    }

    chat_tree
        .chat_tree
        .remove(&make_msg_key(guild_id, channel_id, message_id))
        .await
        .map_err(ServerError::DbError)?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::DeletedMessage(stream_event::MessageDeleted {
            guild_id,
            channel_id,
            message_id,
        }),
        Some(PermCheck::new(
            guild_id,
            Some(channel_id),
            "messages.view",
            false,
        )),
        EventContext::empty(),
    );

    Ok((DeleteMessageResponse {}).into_response())
}
