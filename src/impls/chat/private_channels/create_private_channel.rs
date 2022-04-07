use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<CreatePrivateChannelRequest>,
) -> ServerResult<Response<CreatePrivateChannelResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let CreatePrivateChannelRequest {
        members: mut users_allowed,
        is_dm,
    } = request.into_message().await?;

    users_allowed.dedup();

    if users_allowed.contains(&user_id) {
        bail!((
            "h.cant-allow-self",
            "you cant add yourself to members for private channel, it is implied"
        ));
    }

    if is_dm && users_allowed.len() != 1 {
        bail!((
            "h.must-have-one-member",
            "private channels must have one member if they are direct message channels"
        ));
    }

    let maybe_dm_channel = if is_dm {
        svc.deps
            .chat_tree
            .get(make_dm_with_user_key(user_id, users_allowed[0]))
            .await?
            .map(db::deser_id)
    } else {
        None
    };

    if let Some(channel_id) = maybe_dm_channel {
        join_private_channel::logic(svc.deps.as_ref(), user_id, channel_id).await?;

        svc.broadcast(
            EventSub::PrivateChannel(channel_id),
            stream_event::Event::UserJoinedPrivateChannel(stream_event::UserJoinedPrivateChannel {
                channel_id,
                user_id,
            }),
            None,
            EventContext::empty(),
        );

        svc.dispatch_private_channel_join(channel_id, user_id)
            .await?;

        return Ok(CreatePrivateChannelResponse::new(channel_id).into_response());
    }

    let channel_id = logic(svc.deps.as_ref(), &users_allowed, user_id, is_dm).await?;

    svc.dispatch_private_channel_join(channel_id, user_id)
        .await?;

    // create invite, using channel_id as invite_id
    svc.deps
        .chat_tree
        .create_priv_invite_logic(channel_id, users_allowed.clone(), true)
        .await?;

    // dispatch invites to users
    for invitee_id in users_allowed {
        svc.dispatch_user_invite_received(
            user_id,
            invitee_id,
            pending_invite::Location::ChannelId(channel_id),
        )
        .await?;
    }

    Ok(CreatePrivateChannelResponse::new(channel_id).into_response())
}

pub async fn logic(
    deps: &Dependencies,
    users_allowed: &[u64],
    creator_id: u64,
    is_dm: bool,
) -> ServerResult<u64> {
    let chat_tree = &deps.chat_tree;

    let channel_id = {
        let mut rng = rand::rngs::SmallRng::from_entropy();
        let mut channel_id = rng.gen_range(1..u64::MAX);
        while chat_tree.contains_key(&make_pc_key(channel_id)).await? {
            channel_id = rng.gen_range(1..u64::MAX);
        }
        channel_id
    };

    let private_channel = PrivateChannel {
        members: vec![creator_id],
        is_dm,
    };
    let serialized = rkyv_ser(&private_channel);

    let mut batch = Batch::default();
    batch.insert(make_pc_creator_key(channel_id), creator_id.to_be_bytes());
    batch.insert(make_pc_key(channel_id), serialized);
    if is_dm {
        batch.insert(
            make_dm_with_user_key(creator_id, users_allowed[0]),
            channel_id.to_be_bytes(),
        );
    }
    chat_tree.apply_batch(batch).await?;

    Ok(channel_id)
}
