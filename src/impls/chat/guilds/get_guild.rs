use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetGuildRequest>,
) -> ServerResult<Response<GetGuildResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let GetGuildRequest { guild_ids } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    for guild_id in guild_ids.iter().copied() {
        chat_tree.check_guild_user(guild_id, user_id).await?;
    }

    let mut guilds = HashMap::with_capacity(guild_ids.len());
    for guild_id in guild_ids {
        let guild = chat_tree.get_guild_logic(guild_id).await?;
        guilds.insert(guild_id, guild);
    }

    Ok(GetGuildResponse::new(guilds).into_response())
}
