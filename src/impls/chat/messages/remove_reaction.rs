use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<RemoveReactionRequest>,
) -> ServerResult<Response<RemoveReactionResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let RemoveReactionRequest {
        guild_id,
        channel_id,
        message_id,
        reaction_data,
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
            all_permissions::MESSAGES_REACTIONS_REMOVE,
            false,
        )
        .await?;

    chat_tree
        .remove_reaction(user_id, guild_id, channel_id, message_id, &reaction_data)
        .await?;
    svc.send_removed_reaction_event(guild_id, channel_id, message_id, reaction_data);

    Ok(RemoveReactionResponse::new().into_response())
}
