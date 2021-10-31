use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<DeleteInviteRequest>,
) -> ServerResult<Response<DeleteInviteResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let DeleteInviteRequest {
        guild_id,
        invite_id,
    } = request.into_message().await?;

    svc.chat_tree.check_guild_user(guild_id, user_id)?;
    svc.chat_tree
        .check_perms(guild_id, None, user_id, "invites.manage.delete", false)?;

    svc.chat_tree
        .chat_tree
        .remove(&make_invite_key(invite_id.as_str()))
        .map_err(ServerError::DbError)?;

    Ok((DeleteInviteResponse {}).into_response())
}
