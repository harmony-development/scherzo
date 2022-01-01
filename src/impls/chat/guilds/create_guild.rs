use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<CreateGuildRequest>,
) -> ServerResult<Response<CreateGuildResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let CreateGuildRequest {
        metadata,
        name,
        picture,
    } = request.into_message().await?;

    let guild_id = svc
        .deps
        .chat_tree
        .create_guild_logic(
            user_id,
            name,
            picture,
            metadata,
            guild_kind::Kind::new_normal(guild_kind::Normal::new()),
        )
        .await?;

    svc.dispatch_guild_join(guild_id, user_id).await?;

    Ok((CreateGuildResponse { guild_id }).into_response())
}
