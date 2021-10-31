use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<DeleteInviteRequest>,
) -> ServerResult<Response<DeleteInviteResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let DeleteInviteRequest {
        guild_id,
        invite_id,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id)?;
    chat_tree.check_perms(guild_id, None, user_id, "invites.manage.delete", false)?;

    chat_tree
        .chat_tree
        .remove(&make_invite_key(invite_id.as_str()))
        .map_err(ServerError::DbError)?;

    Ok((DeleteInviteResponse {}).into_response())
}
