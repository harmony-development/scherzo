use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<MoveRoleRequest>,
) -> ServerResult<Response<MoveRoleResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let MoveRoleRequest {
        guild_id,
        role_id,
        new_position,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "roles.manage", false)
        .await?;
    chat_tree.does_role_exist(guild_id, role_id).await?;

    if let Some(pos) = new_position {
        chat_tree
            .move_role_logic(guild_id, role_id, Some(pos.clone()))
            .await?;
        svc.broadcast(
            EventSub::Guild(guild_id),
            stream_event::Event::RoleMoved(stream_event::RoleMoved {
                guild_id,
                role_id,
                new_position: Some(pos),
            }),
            Some(PermCheck::new(
                guild_id,
                None,
                all_permissions::ROLES_GET,
                false,
            )),
            EventContext::empty(),
        );
    }

    Ok(MoveRoleResponse::new().into_response())
}
