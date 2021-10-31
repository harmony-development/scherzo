use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<KickUserRequest>,
) -> ServerResult<Response<KickUserResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let KickUserRequest {
        guild_id,
        user_id: user_to_kick,
    } = request.into_message().await?;

    if user_id == user_to_kick {
        return Err(ServerError::CantKickOrBanYourself.into());
    }

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id)?;
    chat_tree.is_user_in_guild(guild_id, user_to_kick)?;
    chat_tree.check_perms(guild_id, None, user_id, "user.manage.kick", false)?;

    chat_tree.kick_user_logic(guild_id, user_to_kick)?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::LeftMember(stream_event::MemberLeft {
            guild_id,
            member_id: user_to_kick,
            leave_reason: LeaveReason::Kicked.into(),
        }),
        None,
        EventContext::empty(),
    );

    svc.dispatch_guild_leave(guild_id, user_to_kick)?;

    Ok((KickUserResponse {}).into_response())
}
