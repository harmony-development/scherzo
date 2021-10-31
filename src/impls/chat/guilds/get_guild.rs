use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<GetGuildRequest>,
) -> ServerResult<Response<GetGuildResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let GetGuildRequest { guild_id } = request.into_message().await?;

    svc.chat_tree.check_guild_user(guild_id, user_id)?;

    svc.chat_tree
        .get_guild_logic(guild_id)
        .map(|g| Response::new(GetGuildResponse { guild: Some(g) }))
}
