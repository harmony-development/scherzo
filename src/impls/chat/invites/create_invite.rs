use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<CreateInviteRequest>,
) -> ServerResult<Response<CreateInviteResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let CreateInviteRequest {
        guild_id,
        name,
        possible_uses,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "invites.manage.create", false)
        .await?;

    chat_tree
        .create_invite_logic(guild_id, name.as_str(), possible_uses)
        .await?;

    Ok((CreateInviteResponse { invite_id: name }).into_response())
}
