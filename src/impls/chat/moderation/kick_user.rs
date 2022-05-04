use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<KickUserRequest>,
) -> ServerResult<Response<KickUserResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let KickUserRequest {
        guild_id,
        user_id: user_to_kick,
    } = request.into_message().await?;

    if user_id == user_to_kick {
        bail!(ServerError::CantKickOrBanYourself);
    }

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_user_in_guild(guild_id, user_to_kick)
        .await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "user.manage.kick", false)
        .await?;

    chat_tree.kick_user_logic(guild_id, user_to_kick).await?;

    svc.broadcast(
        EventSub::Guild(guild_id),
        stream_event::Event::LeftMember(stream_event::MemberLeft {
            guild_id,
            member_id: user_to_kick,
            leave_reason: LeaveReason::Kicked.into(),
        }),
        None,
        EventContext::empty(),
    );

    svc.dispatch_guild_leave(guild_id, user_to_kick).await?;

    Ok(KickUserResponse::new().into_response())
}
