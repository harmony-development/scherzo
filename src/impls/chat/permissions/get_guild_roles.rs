use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<GetGuildRolesRequest>,
) -> ServerResult<Response<GetGuildRolesResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let GetGuildRolesRequest { guild_id } = request.into_message().await?;

    svc.chat_tree.check_guild_user(guild_id, user_id)?;
    svc.chat_tree
        .check_perms(guild_id, None, user_id, "roles.get", false)?;

    let roles = svc.chat_tree.get_guild_roles_logic(guild_id)?;

    Ok((GetGuildRolesResponse { roles }).into_response())
}
