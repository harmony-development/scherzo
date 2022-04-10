use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<UpdatePrivateChannelMembersRequest>,
) -> ServerResult<Response<UpdatePrivateChannelMembersResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let UpdatePrivateChannelMembersRequest {
        channel_id,
        added_members,
        removed_members,
    } = request.into_message().await?;

    svc.deps
        .chat_tree
        .check_private_channel_user(channel_id, user_id)
        .await?;

    let (added, removed) = logic(svc, channel_id, added_members, removed_members).await?;

    // dispatch invites to new members
    for invitee_id in added {
        svc.dispatch_user_invite_received(
            user_id,
            invitee_id,
            pending_invite::Location::ChannelId(channel_id),
        )
        .await?;
    }

    // dispatch left events to removed users
    for member in removed {
        svc.dispatch_private_channel_leave(channel_id, member)
            .await?;
    }

    Ok(UpdatePrivateChannelMembersResponse::new().into_response())
}

pub async fn logic(
    svc: &ChatServer,
    channel_id: u64,
    added: Vec<u64>,
    removed: Vec<u64>,
) -> ServerResult<(Vec<u64>, Vec<u64>)> {
    let mut private_channel = get_private_channel::logic(svc.deps.as_ref(), channel_id).await?;

    if private_channel.is_dm {
        bail!((
            "h.cant-update-members",
            "private channel is a direct message channel, cant change members"
        ));
    }

    let members_before = private_channel.members.clone();

    private_channel
        .members
        .retain(|id| removed.contains(id).not());
    private_channel.members.extend(added);
    private_channel.members.dedup();

    // members_before: [1, 2]
    // private_channel.members: [1, 2, 3]
    // members_added: [3]
    let members_added = private_channel
        .members
        .iter()
        .filter_map(|id| members_before.contains(id).not().then(|| *id))
        .collect::<Vec<_>>();

    // members_before: [1, 2, 3, 4]
    // private_channel.members: [2, 3, 4]
    // members_removed: [1]
    let members_removed = members_before
        .iter()
        .filter_map(|id| private_channel.members.contains(id).not().then(|| *id))
        .collect::<Vec<_>>();

    // remove added members, they didnt join yet
    private_channel
        .members
        .retain(|id| members_added.contains(id).not());

    // insert new channel
    let serialized = rkyv_ser(&private_channel);
    svc.deps
        .chat_tree
        .insert(make_pc_key(channel_id), serialized)
        .await?;

    // update invite allowed users
    svc.deps
        .chat_tree
        .update_priv_invite_allowed_users(channel_id.to_string(), |allowed_users| {
            allowed_users.retain(|id| members_removed.contains(id).not());
            allowed_users.extend(members_added.iter().copied());
        })
        .await?;

    Ok((members_added, members_removed))
}
