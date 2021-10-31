use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<ManageUserRolesRequest>,
) -> ServerResult<Response<ManageUserRolesResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let ManageUserRolesRequest {
        guild_id,
        user_id: user_to_manage,
        give_role_ids,
        take_role_ids,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id)?;
    chat_tree.is_user_in_guild(guild_id, user_to_manage)?;
    chat_tree.check_perms(guild_id, None, user_id, "roles.user.manage", false)?;
    let user_to_manage = if user_to_manage != 0 {
        user_to_manage
    } else {
        user_id
    };

    let new_role_ids = chat_tree.manage_user_roles_logic(
        guild_id,
        user_to_manage,
        give_role_ids,
        take_role_ids,
    )?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::UserRolesUpdated(stream_event::UserRolesUpdated {
            guild_id,
            user_id: user_to_manage,
            new_role_ids,
        }),
        None,
        EventContext::empty(),
    );

    Ok((ManageUserRolesResponse {}).into_response())
}
