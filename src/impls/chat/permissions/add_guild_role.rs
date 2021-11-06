use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<AddGuildRoleRequest>,
) -> ServerResult<Response<AddGuildRoleResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let AddGuildRoleRequest {
        guild_id,
        name,
        color,
        hoist,
        pingable,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id)?;
    chat_tree.check_perms(guild_id, None, user_id, "roles.manage", false)?;

    let role = Role {
        name: name.clone(),
        color,
        hoist,
        pingable,
    };
    let role_id = chat_tree.add_guild_role_logic(guild_id, None, role)?;
    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::RoleCreated(stream_event::RoleCreated {
            guild_id,
            role_id,
            name,
            color,
            hoist,
            pingable,
        }),
        None,
        EventContext::empty(),
    );

    Ok((AddGuildRoleResponse { role_id }).into_response())
}
