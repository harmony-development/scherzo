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

    let channel_id = logic(svc.deps.as_ref(), user_id, is_dm).await?;

    // TODO: add logic for detecting an already existing dm for user with the other user

    users_allowed.dedup();

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

pub async fn logic(deps: &Dependencies, creator_id: u64, is_dm: bool) -> ServerResult<u64> {
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
    chat_tree.apply_batch(batch).await?;

    Ok(channel_id)
}
