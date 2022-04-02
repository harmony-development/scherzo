use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<ManageUserRolesRequest>,
) -> ServerResult<Response<ManageUserRolesResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let ManageUserRolesRequest {
        guild_id,
        user_id: user_to_manage,
        give_role_ids,
        take_role_ids,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_user_in_guild(guild_id, user_to_manage)
        .await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "roles.user.manage", false)
        .await?;
    let user_to_manage = if user_to_manage != 0 {
        user_to_manage
    } else {
        user_id
    };

    let new_role_ids = chat_tree
        .manage_user_roles_logic(guild_id, user_to_manage, give_role_ids, take_role_ids)
        .await?;

    svc.broadcast(
        EventSub::Guild(guild_id),
        stream_event::Event::UserRolesUpdated(stream_event::UserRolesUpdated {
            guild_id,
            user_id: user_to_manage,
            new_role_ids,
        }),
        Some(PermCheck::new(guild_id, None, "roles.user.get", false)),
        EventContext::empty(),
    );

    Ok(ManageUserRolesResponse::new().into_response())
}
