use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<CreateInviteRequest>,
) -> ServerResult<Response<CreateInviteResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let CreateInviteRequest {
        guild_id,
        name,
        possible_uses,
    } = request.into_message().await?;

    svc.chat_tree.check_guild_user(guild_id, user_id)?;
    svc.chat_tree
        .check_perms(guild_id, None, user_id, "invites.manage.create", false)?;

    svc.chat_tree
        .create_invite_logic(guild_id, name.as_str(), possible_uses)?;

    Ok((CreateInviteResponse { invite_id: name }).into_response())
}
