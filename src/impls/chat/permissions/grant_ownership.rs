use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GrantOwnershipRequest>,
) -> ServerResult<Response<GrantOwnershipResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let GrantOwnershipRequest {
        new_owner_id,
        guild_id,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id)?;
    chat_tree.is_user_in_guild(guild_id, new_owner_id)?;

    chat_tree.check_perms(guild_id, None, user_id, "", true)?;

    let mut guild = chat_tree.get_guild_logic(guild_id)?;
    guild.owner_ids.push(new_owner_id);
    chat_tree.put_guild_logic(guild_id, guild)?;

    Ok((GrantOwnershipResponse {}).into_response())
}
