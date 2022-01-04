use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetPermissionsRequest>,
) -> ServerResult<Response<GetPermissionsResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let GetPermissionsRequest {
        guild_id,
        channel_id,
        role_id,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_perms(
            guild_id,
            channel_id,
            user_id,
            "permissions.manage.get",
            false,
        )
        .await?;

    let perms = chat_tree
        .get_permissions_logic(guild_id, channel_id, role_id)
        .await?
        .into_iter()
        .map(|(m, ok)| Permission {
            matches: m.into(),
            ok,
        })
        .collect();

    Ok((GetPermissionsResponse { perms }).into_response())
}
