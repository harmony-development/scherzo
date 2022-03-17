use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<BanUserRequest>,
) -> ServerResult<Response<BanUserResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let BanUserRequest {
        guild_id,
        user_id: user_to_ban,
    } = request.into_message().await?;

    if user_id == user_to_ban {
        return Err(ServerError::CantKickOrBanYourself.into());
    }

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree.is_user_in_guild(guild_id, user_to_ban).await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "user.manage.ban", false)
        .await?;

    chat_tree.kick_user_logic(guild_id, user_to_ban).await?;

    chat_tree
        .insert(
            make_banned_member_key(guild_id, user_to_ban),
            get_time_secs().to_be_bytes(),
        )
        .await?;

    svc.broadcast(
        EventSub::Guild(guild_id),
        stream_event::Event::LeftMember(stream_event::MemberLeft {
            guild_id,
            member_id: user_to_ban,
            leave_reason: LeaveReason::Banned.into(),
        }),
        None,
        EventContext::empty(),
    );

    svc.dispatch_guild_leave(guild_id, user_to_ban).await?;

    Ok(BanUserResponse::new().into_response())
}
