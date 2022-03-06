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

    if name.is_empty() {
        bail!(("h.bad-guild-name", "guild name can't be empty"));
    }

    if picture.as_ref().map_or(false, String::is_empty) {
        bail!(("h.bad-guild-picture", "guild picture can't be empty if set"));
    }

    let guild_id = svc
        .deps
        .chat_tree
        .create_guild_logic(
            user_id,
            name,
            picture,
            metadata,
            guild::Kind::NormalUnspecified,
        )
        .await?;

    svc.dispatch_guild_join(guild_id, user_id).await?;

    Ok((CreateGuildResponse { guild_id }).into_response())
}
