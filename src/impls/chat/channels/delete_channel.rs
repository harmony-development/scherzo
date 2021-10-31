use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<DeleteChannelRequest>,
) -> ServerResult<Response<DeleteChannelResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let DeleteChannelRequest {
        guild_id,
        channel_id,
    } = request.into_message().await?;

    svc.chat_tree
        .check_guild_user_channel(guild_id, user_id, channel_id)?;
    svc.chat_tree.check_perms(
        guild_id,
        Some(channel_id),
        user_id,
        "channels.manage.delete",
        false,
    )?;

    let channel_data = svc
        .chat_tree
        .chat_tree
        .scan_prefix(&make_chan_key(guild_id, channel_id))
        .try_fold(Vec::new(), |mut all, res| {
            all.push(res.map_err(ServerError::from)?.0);
            ServerResult::Ok(all)
        })?;

    // Remove from ordering list
    let key = make_guild_chan_ordering_key(guild_id);
    let mut ordering = svc.chat_tree.get_list_u64_logic(&key)?;
    if let Some(index) = ordering.iter().position(|oid| channel_id.eq(oid)) {
        ordering.remove(index);
    } else {
        unreachable!("all valid channel IDs are valid ordering IDs");
    }
    let serialized_ordering = svc.chat_tree.serialize_list_u64_logic(ordering);

    let mut batch = Batch::default();
    for key in channel_data {
        batch.remove(key);
    }
    batch.insert(key, serialized_ordering);
    svc.chat_tree
        .chat_tree
        .apply_batch(batch)
        .map_err(ServerError::DbError)?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::DeletedChannel(stream_event::ChannelDeleted {
            guild_id,
            channel_id,
        }),
        None,
        EventContext::empty(),
    );

    Ok((DeleteChannelResponse {}).into_response())
}
