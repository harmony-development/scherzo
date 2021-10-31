use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<GetMessageRequest>,
) -> ServerResult<Response<GetMessageResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let request = request.into_message().await?;

    let GetMessageRequest {
        guild_id,
        channel_id,
        message_id,
    } = request;

    svc.chat_tree
        .check_guild_user_channel(guild_id, user_id, channel_id)?;
    svc.chat_tree
        .check_perms(guild_id, Some(channel_id), user_id, "messages.view", false)?;

    let message = Some(
        svc.chat_tree
            .get_message_logic(guild_id, channel_id, message_id)?
            .0,
    );

    Ok((GetMessageResponse { message }).into_response())
}
