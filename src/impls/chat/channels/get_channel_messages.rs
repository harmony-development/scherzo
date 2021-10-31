use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<GetChannelMessagesRequest>,
) -> ServerResult<Response<GetChannelMessagesResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let GetChannelMessagesRequest {
        guild_id,
        channel_id,
        message_id,
        direction,
        count,
    } = request.into_message().await?;

    svc.chat_tree
        .check_guild_user_channel(guild_id, user_id, channel_id)?;
    svc.chat_tree
        .check_perms(guild_id, Some(channel_id), user_id, "messages.view", false)?;

    svc.chat_tree
        .get_channel_messages_logic(
            guild_id,
            channel_id,
            message_id,
            direction.map(|val| Direction::from_i32(val).unwrap_or_default()),
            count,
        )
        .map(Response::new)
}
