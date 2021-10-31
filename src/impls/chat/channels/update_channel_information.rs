use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<UpdateChannelInformationRequest>,
) -> ServerResult<Response<UpdateChannelInformationResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let UpdateChannelInformationRequest {
        guild_id,
        channel_id,
        new_name,
        new_metadata,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id)?;
    chat_tree.check_perms(
        guild_id,
        Some(channel_id),
        user_id,
        "channels.manage.change-information",
        false,
    )?;

    let key = make_chan_key(guild_id, channel_id);
    let mut chan_info = if let Some(raw) = chat_tree.get(key)? {
        db::deser_chan(raw)
    } else {
        return Err(ServerError::NoSuchChannel {
            guild_id,
            channel_id,
        }
        .into());
    };

    if let Some(new_name) = new_name.clone() {
        chan_info.channel_name = new_name;
    }
    if let Some(new_metadata) = new_metadata.clone() {
        chan_info.metadata = Some(new_metadata);
    }

    let buf = rkyv_ser(&chan_info);
    chat_tree.insert(key, buf)?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::EditedChannel(stream_event::ChannelUpdated {
            guild_id,
            channel_id,
            new_name,
            new_metadata,
        }),
        Some(PermCheck::new(
            guild_id,
            Some(channel_id),
            "messages.view",
            false,
        )),
        EventContext::empty(),
    );

    Ok((UpdateChannelInformationResponse {}).into_response())
}
