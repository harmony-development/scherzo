use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<MoveRoleRequest>,
) -> ServerResult<Response<MoveRoleResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let MoveRoleRequest {
        guild_id,
        role_id,
        new_position,
    } = request.into_message().await?;

    svc.chat_tree.check_guild_user(guild_id, user_id)?;
    svc.chat_tree
        .check_perms(guild_id, None, user_id, "roles.manage", false)?;
    svc.chat_tree.does_role_exist(guild_id, role_id)?;

    if let Some(pos) = new_position {
        svc.chat_tree
            .move_role_logic(guild_id, role_id, Some(pos.clone()))?;
        svc.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::RoleMoved(stream_event::RoleMoved {
                guild_id,
                role_id,
                new_position: Some(pos),
            }),
            None,
            EventContext::empty(),
        );
    }

    Ok((MoveRoleResponse {}).into_response())
}
