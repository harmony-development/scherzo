use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<JoinGuildRequest>,
) -> ServerResult<Response<JoinGuildResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let JoinGuildRequest { invite_id } = request.into_message().await?;
    let key = make_invite_key(invite_id.as_str());

    let chat_tree = &svc.deps.chat_tree;

    let (guild_id, mut invite) = if let Some(raw) = chat_tree.get(&key).await? {
        db::deser_invite_entry(raw)
    } else {
        bail!(ServerError::NoSuchInvite(invite_id.into()));
    };

    if chat_tree.is_user_banned_in_guild(guild_id, user_id).await? {
        bail!(ServerError::UserBanned);
    }

    chat_tree
        .check_user_in_guild(guild_id, user_id)
        .await
        .ok()
        .map_or(Ok(()), |_| Err(ServerError::UserAlreadyInGuild))?;

    let is_infinite = invite.possible_uses == 0;

    if is_infinite.not() && invite.use_count >= invite.possible_uses {
        return Err(ServerError::InviteExpired.into());
    }

    chat_tree
        .insert(make_member_key(guild_id, user_id), [])
        .await?;
    chat_tree.add_default_role_to(guild_id, user_id).await?;
    invite.use_count += 1;

    let is_invite_consumed = is_infinite.not() && invite.use_count >= invite.possible_uses;
    if is_invite_consumed {
        chat_tree.delete_invite_logic(invite_id).await?;
    }

    svc.broadcast(
        EventSub::Guild(guild_id),
        stream_event::Event::JoinedMember(stream_event::MemberJoined {
            guild_id,
            member_id: user_id,
        }),
        None,
        EventContext::empty(),
    );

    svc.dispatch_guild_join(guild_id, user_id).await?;

    if !is_invite_consumed {
        let buf = rkyv_ser(&invite);
        chat_tree
            .insert(
                key,
                [guild_id.to_be_bytes().as_ref(), buf.as_ref()].concat(),
            )
            .await?;
    }

    Ok((JoinGuildResponse { guild_id }).into_response())
}
