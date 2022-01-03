use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<DeleteInviteRequest>,
) -> ServerResult<Response<DeleteInviteResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let DeleteInviteRequest {
        guild_id,
        invite_id,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "invites.manage.delete", false)
        .await?;

    chat_tree.delete_invite_logic(invite_id).await?;

    Ok((DeleteInviteResponse {}).into_response())
}
