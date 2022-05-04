use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<UnbanUserRequest>,
) -> ServerResult<Response<UnbanUserResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let UnbanUserRequest {
        guild_id,
        user_id: user_to_unban,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "user.manage.unban", false)
        .await?;

    chat_tree
        .remove(make_banned_member_key(guild_id, user_to_unban))
        .await?;

    Ok(UnbanUserResponse::new().into_response())
}
