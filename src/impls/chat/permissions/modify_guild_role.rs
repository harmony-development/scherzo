use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<ModifyGuildRoleRequest>,
) -> ServerResult<Response<ModifyGuildRoleResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let ModifyGuildRoleRequest {
        guild_id,
        role_id,
        new_name,
        new_color,
        new_hoist,
        new_pingable,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "roles.manage", false)
        .await?;

    let key = make_guild_role_key(guild_id, role_id);
    let mut role = if let Some(raw) = chat_tree.get(key).await? {
        db::deser_role(raw)
    } else {
        return Err(ServerError::NoSuchRole { guild_id, role_id }.into());
    };

    if let Some(new_name) = new_name.clone() {
        role.name = new_name;
    }
    if let Some(new_color) = new_color {
        role.color = new_color;
    }
    if let Some(new_hoist) = new_hoist {
        role.hoist = new_hoist;
    }
    if let Some(new_pingable) = new_pingable {
        role.pingable = new_pingable;
    }

    let ser_role = rkyv_ser(&role);
    chat_tree.insert(key, ser_role).await?;

    svc.broadcast(
        EventSub::Guild(guild_id),
        stream_event::Event::RoleUpdated(stream_event::RoleUpdated {
            guild_id,
            role_id,
            new_name,
            new_color,
            new_hoist,
            new_pingable,
        }),
        Some(PermCheck::new(
            guild_id,
            None,
            all_permissions::ROLES_GET,
            false,
        )),
        EventContext::empty(),
    );

    Ok(ModifyGuildRoleResponse::new().into_response())
}
