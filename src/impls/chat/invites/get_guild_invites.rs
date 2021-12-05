use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetGuildInvitesRequest>,
) -> ServerResult<Response<GetGuildInvitesResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let GetGuildInvitesRequest { guild_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "invites.view", false)
        .await?;

    chat_tree
        .get_guild_invites_logic(guild_id)
        .await
        .map(IntoResponse::into_response)
}
