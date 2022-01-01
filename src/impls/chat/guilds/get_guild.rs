use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetGuildRequest>,
) -> ServerResult<Response<GetGuildResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let GetGuildRequest { guild_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;

    chat_tree
        .get_guild_logic(guild_id)
        .await
        .map(|g| (GetGuildResponse { guild: Some(g) }).into_response())
}
