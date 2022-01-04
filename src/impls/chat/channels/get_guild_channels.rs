use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetGuildChannelsRequest>,
) -> ServerResult<Response<GetGuildChannelsResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let GetGuildChannelsRequest { guild_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;

    chat_tree
        .get_guild_channels_logic(guild_id, user_id)
        .await
        .map(IntoResponse::into_response)
        .map_err(Into::into)
}
