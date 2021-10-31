use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<GetGuildChannelsRequest>,
) -> ServerResult<Response<GetGuildChannelsResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let GetGuildChannelsRequest { guild_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id)?;

    chat_tree
        .get_guild_channels_logic(guild_id, user_id)
        .map(Response::new)
}
