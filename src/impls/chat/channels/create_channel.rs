use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<CreateChannelRequest>,
) -> ServerResult<Response<CreateChannelResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let CreateChannelRequest {
        guild_id,
        channel_name,
        kind,
        position,
        metadata,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id)?;
    chat_tree.check_perms(guild_id, None, user_id, "channels.manage.create", false)?;

    let channel_id = chat_tree.create_channel_logic(
        guild_id,
        channel_name.clone(),
        ChannelKind::from_i32(kind).unwrap_or_default(),
        metadata.clone(),
        position.clone(),
    )?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::CreatedChannel(stream_event::ChannelCreated {
            guild_id,
            channel_id,
            name: channel_name,
            position,
            kind,
            metadata,
        }),
        Some(PermCheck::new(
            guild_id,
            Some(channel_id),
            "messages.view",
            false,
        )),
        EventContext::empty(),
    );

    Ok((CreateChannelResponse { channel_id }).into_response())
}
