use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<GetGuildChannelsRequest>,
) -> ServerResult<Response<GetGuildChannelsResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let GetGuildChannelsRequest { guild_id } = request.into_message().await?;

    svc.chat_tree.check_guild_user(guild_id, user_id)?;

    svc.chat_tree
        .get_guild_channels_logic(guild_id, user_id)
        .map(Response::new)
}
