use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<GetGuildMembersRequest>,
) -> ServerResult<Response<GetGuildMembersResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let GetGuildMembersRequest { guild_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id)?;

    chat_tree
        .get_guild_members_logic(guild_id)
        .map(Response::new)
}
