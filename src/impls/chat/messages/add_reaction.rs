use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<AddReactionRequest>,
) -> ServerResult<Response<AddReactionResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let AddReactionRequest {
        guild_id,
        channel_id,
        message_id,
        emote,
    } = request.into_message().await?;

    if let Some(emote) = emote {
        let chat_tree = &svc.deps.chat_tree;

        chat_tree.check_perms(
            guild_id,
            Some(channel_id),
            user_id,
            all_permissions::MESSAGES_REACTIONS_ADD,
            false,
        )?;

        let reaction =
            chat_tree.update_reaction(user_id, guild_id, channel_id, message_id, emote, true)?;
        svc.send_reaction_event(guild_id, channel_id, message_id, reaction);
    }

    Ok((AddReactionResponse {}).into_response())
}
