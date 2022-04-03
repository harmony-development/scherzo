use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetChannelMessagesRequest>,
) -> ServerResult<Response<GetChannelMessagesResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let GetChannelMessagesRequest {
        guild_id,
        channel_id,
        message_id,
        direction,
        count,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree
        .check_channel_user(guild_id, user_id, channel_id)
        .await?;
    chat_tree
        .check_perms(guild_id, Some(channel_id), user_id, "messages.view", false)
        .await?;

    chat_tree
        .get_channel_messages_logic(
            guild_id,
            channel_id,
            message_id,
            direction.map(|val| Direction::from_i32(val).unwrap_or_default()),
            count,
        )
        .await
        .map(IntoResponse::into_response)
}
