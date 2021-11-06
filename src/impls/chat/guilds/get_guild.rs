use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetGuildRequest>,
) -> ServerResult<Response<GetGuildResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let GetGuildRequest { guild_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id)?;

    chat_tree
        .get_guild_logic(guild_id)
        .map(|g| Response::new(GetGuildResponse { guild: Some(g) }))
}
