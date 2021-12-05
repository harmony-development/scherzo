use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<UpdateAllChannelOrderRequest>,
) -> ServerResult<Response<UpdateAllChannelOrderResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let UpdateAllChannelOrderRequest {
        guild_id,
        channel_ids,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "channels.manage.move", false)
        .await?;

    for channel_id in &channel_ids {
        chat_tree.does_channel_exist(guild_id, *channel_id).await?;
    }

    let prefix = make_guild_chan_prefix(guild_id);
    let channels = chat_tree
        .scan_prefix(&prefix)
        .await
        .try_fold(Vec::new(), |mut all, res| {
            let (key, _) = res?;
            if key.len() == prefix.len() + size_of::<u64>() {
                all.push(unsafe { u64::from_be_bytes(key.try_into().unwrap_unchecked()) });
            }
            ServerResult::Ok(all)
        })?;

    for channel_id in channels {
        if !channel_ids.contains(&channel_id) {
            return Err(ServerError::UnderSpecifiedChannels.into());
        }
    }

    let key = make_guild_chan_ordering_key(guild_id);
    let serialized_ordering = chat_tree.serialize_list_u64_logic(channel_ids.clone());
    chat_tree.insert(key, serialized_ordering).await?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::ChannelsReordered(stream_event::ChannelsReordered {
            guild_id,
            channel_ids,
        }),
        None,
        EventContext::empty(),
    );

    Ok((UpdateAllChannelOrderResponse {}).into_response())
}
