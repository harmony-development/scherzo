use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<AddReactionRequest>,
) -> ServerResult<Response<AddReactionResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let AddReactionRequest {
        guild_id,
        channel_id,
        message_id,
        reaction_data,
        reaction_kind,
        reaction_name,
    } = request.into_message().await?;
    let reaction_kind = ReactionKind::from_i32(reaction_kind).unwrap_or_default();

    let chat_tree = &svc.deps.chat_tree;

    chat_tree
        .check_perms(
            guild_id,
            Some(channel_id),
            user_id,
            all_permissions::MESSAGES_REACTIONS_ADD,
            false,
        )
        .await?;

    let did_add = chat_tree
        .add_reaction(
            user_id,
            guild_id,
            channel_id,
            message_id,
            reaction_data.clone(),
            reaction_name.clone(),
            reaction_kind,
        )
        .await?;
    if did_add {
        svc.send_new_reaction_event(
            guild_id,
            channel_id,
            message_id,
            Reaction {
                count: 1,
                data: reaction_data,
                kind: reaction_kind.into(),
                name: reaction_name,
            },
        );
    } else {
        svc.send_added_reaction_event(guild_id, channel_id, message_id, reaction_data);
    }

    Ok(AddReactionResponse::new().into_response())
}
