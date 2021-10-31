use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<AddReactionRequest>,
) -> ServerResult<Response<AddReactionResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let AddReactionRequest {
        guild_id,
        channel_id,
        message_id,
        emote,
    } = request.into_message().await?;

    if let Some(emote) = emote {
        svc.chat_tree.check_perms(
            guild_id,
            Some(channel_id),
            user_id,
            all_permissions::MESSAGES_REACTIONS_ADD,
            false,
        )?;

        let reaction = svc
            .chat_tree
            .update_reaction(user_id, guild_id, channel_id, message_id, emote, true)?;
        svc.send_reaction_event(guild_id, channel_id, message_id, reaction);
    }

    Ok((AddReactionResponse {}).into_response())
}
