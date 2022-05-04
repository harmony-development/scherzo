use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<AddGuildRoleRequest>,
) -> ServerResult<Response<AddGuildRoleResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.deps.auth(&request).await?;

    let AddGuildRoleRequest {
        guild_id,
        name,
        color,
        hoist,
        pingable,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "roles.manage", false)
        .await?;

    let role = Role {
        name: name.clone(),
        color,
        hoist,
        pingable,
    };
    let role_id = chat_tree.add_guild_role_logic(guild_id, None, role).await?;
    svc.broadcast(
        EventSub::Guild(guild_id),
        stream_event::Event::RoleCreated(stream_event::RoleCreated {
            guild_id,
            role_id,
            name,
            color,
            hoist,
            pingable,
        }),
        Some(PermCheck::new(guild_id, None, all_permissions::ROLES_GET)),
        EventContext::empty(),
    );

    Ok((AddGuildRoleResponse { role_id }).into_response())
}
