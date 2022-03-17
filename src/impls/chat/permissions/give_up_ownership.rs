use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GiveUpOwnershipRequest>,
) -> ServerResult<Response<GiveUpOwnershipResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let GiveUpOwnershipRequest { guild_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;

    chat_tree
        .check_perms(guild_id, None, user_id, "", true)
        .await?;

    let mut guild = chat_tree.get_guild_logic(guild_id).await?;
    if guild.owner_ids.len() > 1 {
        if let Some(pos) = guild.owner_ids.iter().position(|id| user_id.eq(id)) {
            guild.owner_ids.remove(pos);
        }
    } else {
        return Err(ServerError::MustNotBeLastOwner.into());
    }

    Ok(GiveUpOwnershipResponse::new().into_response())
}
