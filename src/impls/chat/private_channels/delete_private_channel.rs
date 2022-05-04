use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<DeletePrivateChannelRequest>,
) -> ServerResult<Response<DeletePrivateChannelResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let DeletePrivateChannelRequest { channel_id } = request.into_message().await?;

    svc.deps
        .chat_tree
        .check_private_channel_creator(channel_id, user_id)
        .await?;

    let deleted_channel = logic(svc.deps.as_ref(), user_id, channel_id).await?;

    for member in deleted_channel.members {
        svc.dispatch_private_channel_leave(channel_id, member)
            .await?;
    }

    broadcast!(
        svc,
        EventSub::PrivateChannel(channel_id),
        PrivateChannelDeleted { channel_id }
    );

    Ok(DeletePrivateChannelResponse::new().into_response())
}

pub async fn logic(
    deps: &Dependencies,
    user_id: u64,
    channel_id: u64,
) -> ServerResult<PrivateChannel> {
    let chat_tree = &deps.chat_tree;

    let creator_id_raw = chat_tree
        .get(make_pc_creator_key(channel_id))
        .await?
        .ok_or(ServerError::NoSuchPrivateChannel(channel_id))?;
    let creator_id = deser_id(creator_id_raw);

    if user_id != creator_id {
        bail!((
            "h.user-not-private-channel-creator",
            "you must be the creator of the private channel to delete it"
        ));
    }

    let private_channel = get_private_channel::logic(deps, channel_id).await?;

    let batch =
        db::create_batch_delete_prefix(&chat_tree.chat_tree, make_pc_key(channel_id)).await?;

    // also delete invite, if any
    let mut batch = batch.merge(
        db::create_batch_delete_prefix(
            &chat_tree.chat_tree,
            make_priv_invite_key(&channel_id.to_string()),
        )
        .await?,
    );

    if private_channel.is_dm {
        batch.remove(make_dm_with_user_key(
            private_channel.members[0],
            private_channel.members[1],
        ));
        batch.remove(make_dm_with_user_key(
            private_channel.members[1],
            private_channel.members[0],
        ));
    }

    chat_tree.apply_batch(batch).await?;

    Ok(private_channel)
}
