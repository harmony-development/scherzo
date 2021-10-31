use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<GrantOwnershipRequest>,
) -> ServerResult<Response<GrantOwnershipResponse>> {
    let user_id = svc.valid_sessions.auth(&request)?;

    let GrantOwnershipRequest {
        new_owner_id,
        guild_id,
    } = request.into_message().await?;

    svc.chat_tree.check_guild_user(guild_id, user_id)?;
    svc.chat_tree.is_user_in_guild(guild_id, new_owner_id)?;

    svc.chat_tree
        .check_perms(guild_id, None, user_id, "", true)?;

    let mut guild = svc.chat_tree.get_guild_logic(guild_id)?;
    guild.owner_ids.push(new_owner_id);
    svc.chat_tree.put_guild_logic(guild_id, guild)?;

    Ok((GrantOwnershipResponse {}).into_response())
}
