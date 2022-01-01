use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetMessageRequest>,
) -> ServerResult<Response<GetMessageResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let request = request.into_message().await?;

    let GetMessageRequest {
        guild_id,
        channel_id,
        message_id,
    } = request;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree
        .check_guild_user_channel(guild_id, user_id, channel_id)
        .await?;
    chat_tree
        .check_perms(guild_id, Some(channel_id), user_id, "messages.view", false)
        .await?;

    let message = Some(
        chat_tree
            .get_message_logic(guild_id, channel_id, message_id)
            .await?
            .0,
    );

    Ok((GetMessageResponse { message }).into_response())
}
