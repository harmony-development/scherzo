use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetUserRolesRequest>,
) -> ServerResult<Response<GetUserRolesResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let GetUserRolesRequest {
        guild_id,
        user_id: user_to_fetch,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree.is_user_in_guild(guild_id, user_to_fetch).await?;
    let fetch_user = (user_to_fetch == 0)
        .then(|| user_id)
        .unwrap_or(user_to_fetch);
    if fetch_user != user_id {
        chat_tree
            .check_perms(guild_id, None, user_id, "roles.user.get", false)
            .await?;
    }

    let roles = chat_tree.get_user_roles_logic(guild_id, fetch_user).await?;

    Ok((GetUserRolesResponse { roles }).into_response())
}
