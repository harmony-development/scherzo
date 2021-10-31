use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<RemoveReactionRequest>,
) -> ServerResult<Response<RemoveReactionResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let RemoveReactionRequest {
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
            all_permissions::MESSAGES_REACTIONS_REMOVE,
            false,
        )?;

        let reaction = svc
            .chat_tree
            .update_reaction(user_id, guild_id, channel_id, message_id, emote, false)?;
        if reaction.is_some() {
            svc.send_reaction_event(guild_id, channel_id, message_id, reaction);
        }
    }

    Ok((RemoveReactionResponse {}).into_response())
}
