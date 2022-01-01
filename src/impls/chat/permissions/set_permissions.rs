use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<SetPermissionsRequest>,
) -> ServerResult<Response<SetPermissionsResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let SetPermissionsRequest {
        guild_id,
        channel_id,
        role_id,
        perms_to_give,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_perms(
            guild_id,
            channel_id,
            user_id,
            "permissions.manage.set",
            false,
        )
        .await?;

    // TODO: fix
    if !perms_to_give.is_empty() {
        chat_tree
            .set_permissions_logic(guild_id, channel_id, role_id, perms_to_give.clone())
            .await?;
        let members = chat_tree.get_guild_members_logic(guild_id).await?.members;
        let guild_owners = chat_tree.get_guild_owners(guild_id).await?;
        let mut for_users = Vec::with_capacity(members.len());
        for user_id in members.iter() {
            if !guild_owners.contains(user_id) {
                let maybe_user = chat_tree
                    .get_user_roles_logic(guild_id, *user_id)
                    .await?
                    .contains(&role_id)
                    .then(|| *user_id);
                if let Some(user_id) = maybe_user {
                    for_users.push(user_id);
                }
            }
        }
        for perm in &perms_to_give {
            svc.send_event_through_chan(
                EventSub::Guild(guild_id),
                stream_event::Event::PermissionUpdated(stream_event::PermissionUpdated {
                    guild_id,
                    channel_id,
                    query: perm.matches.clone(),
                    ok: perm.ok,
                }),
                None,
                EventContext::new(for_users.clone()),
            );
        }
        svc.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::RolePermsUpdated(stream_event::RolePermissionsUpdated {
                guild_id,
                channel_id,
                role_id,
                new_perms: perms_to_give,
            }),
            Some(PermCheck {
                guild_id,
                channel_id: None,
                check_for: "guild.manage",
                must_be_guild_owner: false,
            }),
            EventContext::empty(),
        );
        Ok((SetPermissionsResponse {}).into_response())
    } else {
        Err(ServerError::NoPermissionsSpecified.into())
    }
}
