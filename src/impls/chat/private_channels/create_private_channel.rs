use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<CreatePrivateChannelRequest>,
) -> ServerResult<Response<CreatePrivateChannelResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let CreatePrivateChannelRequest { members, is_locked } = request.into_message().await?;

    let channel_id = logic(svc.deps.as_ref(), user_id, members.clone(), is_locked).await?;
    let users_allowed = members;

    svc.dispatch_private_channel_join(channel_id, user_id)
        .await?;

    // create invite, using channel_id as invite_id
    svc.deps
        .chat_tree
        .create_priv_invite_logic(channel_id, users_allowed.clone(), true)
        .await?;

    // insert outgoing invites for user
    let mut user_outgoing_invites = Vec::with_capacity(users_allowed.len());
    for invitee_id in users_allowed.iter().copied() {
        let (invitee_id, server_id) = svc
            .deps
            .profile_tree
            .local_to_foreign_id(invitee_id)
            .await?
            .map_or((invitee_id, None), |(id, server_id)| {
                (id, Some(server_id.to_string()))
            });

        let invite = OutgoingInvite {
            invitee_id,
            server_id,
            location: Some(outgoing_invite::Location::ChannelId(channel_id)),
        };

        user_outgoing_invites.push(invite);
    }
    svc.deps
        .chat_tree
        .modify_user_outgoing_invites(user_id, |invites| Ok(invites.extend(user_outgoing_invites)))
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
    creator_id: u64,
    mut members: Vec<u64>,
    is_locked: bool,
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

    // add creator to members since it might not be in the members list
    members.push(creator_id);
    // deduplicate user ids
    members.dedup();

    let private_channel = PrivateChannel { members, is_locked };
    let serialized = rkyv_ser(&private_channel);

    let mut batch = Batch::default();
    batch.insert(make_pc_creator_key(channel_id), creator_id.to_be_bytes());
    batch.insert(make_pc_key(channel_id), serialized);
    chat_tree.apply_batch(batch).await?;

    Ok(channel_id)
}
