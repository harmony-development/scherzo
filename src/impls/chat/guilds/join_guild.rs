use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<JoinGuildRequest>,
) -> ServerResult<Response<JoinGuildResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let JoinGuildRequest { invite_id } = request.into_message().await?;
    let key = make_invite_key(invite_id.as_str());

    let (guild_id, mut invite) = if let Some(raw) = svc.chat_tree.get(&key)? {
        db::deser_invite_entry(raw)
    } else {
        return Err(ServerError::NoSuchInvite(invite_id.into()).into());
    };

    if svc.chat_tree.is_user_banned_in_guild(guild_id, user_id)? {
        return Err(ServerError::UserBanned.into());
    }

    svc.chat_tree
        .is_user_in_guild(guild_id, user_id)
        .ok()
        .map_or(Ok(()), |_| Err(ServerError::UserAlreadyInGuild))?;

    let is_infinite = invite.possible_uses == 0;

    if is_infinite.not() && invite.use_count >= invite.possible_uses {
        return Err(ServerError::InviteExpired.into());
    }

    svc.chat_tree
        .insert(make_member_key(guild_id, user_id), [])?;
    svc.chat_tree.add_default_role_to(guild_id, user_id)?;
    invite.use_count += 1;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::JoinedMember(stream_event::MemberJoined {
            guild_id,
            member_id: user_id,
        }),
        None,
        EventContext::empty(),
    );

    svc.dispatch_guild_join(guild_id, user_id)?;

    let buf = rkyv_ser(&invite);
    svc.chat_tree.insert(
        key,
        [guild_id.to_be_bytes().as_ref(), buf.as_ref()].concat(),
    )?;

    Ok((JoinGuildResponse { guild_id }).into_response())
}