use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<DeleteGuildRoleRequest>,
) -> ServerResult<Response<DeleteGuildRoleResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let DeleteGuildRoleRequest { guild_id, role_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "roles.manage", false)
        .await?;

    chat_tree
        .chat_tree
        .remove(&make_guild_role_key(guild_id, role_id))
        .await
        .map_err(ServerError::DbError)?
        .ok_or(ServerError::NoSuchRole { guild_id, role_id })?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::RoleDeleted(stream_event::RoleDeleted { guild_id, role_id }),
        None,
        EventContext::empty(),
    );

    Ok((DeleteGuildRoleResponse {}).into_response())
}
