use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<DeleteChannelRequest>,
) -> ServerResult<Response<DeleteChannelResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let DeleteChannelRequest {
        guild_id,
        channel_id,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree
        .check_guild_user_channel(guild_id, user_id, channel_id)
        .await?;
    chat_tree
        .check_perms(
            guild_id,
            Some(channel_id),
            user_id,
            "channels.manage.delete",
            false,
        )
        .await?;

    let channel_data = chat_tree
        .scan_prefix(&make_chan_key(guild_id, channel_id))
        .await
        .try_fold(Vec::new(), |mut all, res| {
            all.push(res?.0);
            ServerResult::Ok(all)
        })?;

    // Remove from ordering list
    let key = make_guild_chan_ordering_key(guild_id);
    let mut ordering = chat_tree.get_list_u64_logic(&key).await?;
    if let Some(index) = ordering.iter().position(|oid| channel_id.eq(oid)) {
        ordering.remove(index);
    }
    let serialized_ordering = chat_tree.serialize_list_u64_logic(ordering);

    let mut batch = Batch::default();
    for key in channel_data {
        batch.remove(key);
    }
    batch.insert(key, serialized_ordering);
    chat_tree
        .chat_tree
        .apply_batch(batch)
        .await
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
