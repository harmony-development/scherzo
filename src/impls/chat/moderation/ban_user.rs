use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<BanUserRequest>,
) -> ServerResult<Response<BanUserResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let BanUserRequest {
        guild_id,
        user_id: user_to_ban,
    } = request.into_message().await?;

    if user_id == user_to_ban {
        return Err(ServerError::CantKickOrBanYourself.into());
    }

    svc.chat_tree.check_guild_user(guild_id, user_id)?;
    svc.chat_tree.is_user_in_guild(guild_id, user_to_ban)?;
    svc.chat_tree
        .check_perms(guild_id, None, user_id, "user.manage.ban", false)?;

    svc.chat_tree.kick_user_logic(guild_id, user_to_ban)?;

    svc.chat_tree.insert(
        make_banned_member_key(guild_id, user_to_ban),
        get_time_secs().to_be_bytes(),
    )?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::LeftMember(stream_event::MemberLeft {
            guild_id,
            member_id: user_to_ban,
            leave_reason: LeaveReason::Banned.into(),
        }),
        None,
        EventContext::empty(),
    );

    svc.dispatch_guild_leave(guild_id, user_to_ban)?;

    Ok((BanUserResponse {}).into_response())
}
