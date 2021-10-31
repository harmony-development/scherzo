use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<UnbanUserRequest>,
) -> ServerResult<Response<UnbanUserResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let UnbanUserRequest {
        guild_id,
        user_id: user_to_unban,
    } = request.into_message().await?;

    svc.chat_tree.check_guild_user(guild_id, user_id)?;
    svc.chat_tree
        .check_perms(guild_id, None, user_id, "user.manage.unban", false)?;

    svc.chat_tree
        .remove(make_banned_member_key(guild_id, user_to_unban))?;

    Ok((UnbanUserResponse {}).into_response())
}
