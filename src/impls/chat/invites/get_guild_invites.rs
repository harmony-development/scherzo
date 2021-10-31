use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<GetGuildInvitesRequest>,
) -> ServerResult<Response<GetGuildInvitesResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let GetGuildInvitesRequest { guild_id } = request.into_message().await?;

    svc.chat_tree.check_guild_user(guild_id, user_id)?;
    svc.chat_tree
        .check_perms(guild_id, None, user_id, "invites.view", false)?;

    svc.chat_tree
        .get_guild_invites_logic(guild_id)
        .map(Response::new)
}
