use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<DeleteGuildRoleRequest>,
) -> ServerResult<Response<DeleteGuildRoleResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let DeleteGuildRoleRequest { guild_id, role_id } = request.into_message().await?;

    svc.chat_tree.check_guild_user(guild_id, user_id)?;
    svc.chat_tree
        .check_perms(guild_id, None, user_id, "roles.manage", false)?;

    svc.chat_tree
        .chat_tree
        .remove(&make_guild_role_key(guild_id, role_id))
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
