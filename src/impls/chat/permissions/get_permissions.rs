use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<GetPermissionsRequest>,
) -> ServerResult<Response<GetPermissionsResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let GetPermissionsRequest {
        guild_id,
        channel_id,
        role_id,
    } = request.into_message().await?;

    svc.chat_tree.check_guild_user(guild_id, user_id)?;
    svc.chat_tree.check_perms(
        guild_id,
        channel_id,
        user_id,
        "permissions.manage.get",
        false,
    )?;

    let perms = svc
        .chat_tree
        .get_permissions_logic(guild_id, channel_id, role_id)?
        .into_iter()
        .map(|(m, ok)| Permission {
            matches: m.into(),
            ok,
        })
        .collect();

    Ok((GetPermissionsResponse { perms }).into_response())
}
