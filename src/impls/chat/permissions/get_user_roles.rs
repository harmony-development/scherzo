use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<GetUserRolesRequest>,
) -> ServerResult<Response<GetUserRolesResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let GetUserRolesRequest {
        guild_id,
        user_id: user_to_fetch,
    } = request.into_message().await?;

    svc.chat_tree.check_guild_user(guild_id, user_id)?;
    svc.chat_tree.is_user_in_guild(guild_id, user_to_fetch)?;
    let fetch_user = (user_to_fetch == 0)
        .then(|| user_id)
        .unwrap_or(user_to_fetch);
    if fetch_user != user_id {
        svc.chat_tree
            .check_perms(guild_id, None, user_id, "roles.user.get", false)?;
    }

    let roles = svc.chat_tree.get_user_roles_logic(guild_id, fetch_user)?;

    Ok((GetUserRolesResponse { roles }).into_response())
}
