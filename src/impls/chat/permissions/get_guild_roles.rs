use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetGuildRolesRequest>,
) -> ServerResult<Response<GetGuildRolesResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let GetGuildRolesRequest { guild_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "roles.get", false)
        .await?;

    let roles = chat_tree.get_guild_roles_logic(guild_id).await?;

    Ok((GetGuildRolesResponse { roles }).into_response())
}
