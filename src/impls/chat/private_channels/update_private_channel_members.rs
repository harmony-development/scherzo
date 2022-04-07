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

    // update invite allowed users
    svc.deps
        .chat_tree
        .update_priv_invite_allowed_users(channel_id.to_string(), |allowed_users| {
            allowed_users.retain(|id| removed.contains(id).not());
            allowed_users.extend(added.iter().copied());
        })
        .await?;

    // insert new outgoing invites for user
    let mut user_outgoing_invites = Vec::with_capacity(added.len());
    for invitee_id in added.iter().copied() {
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
    let chat_tree = &svc.deps.chat_tree;

    let mut private_channel = chat_tree.get_private_channel_logic(channel_id).await?;

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

    Ok((members_added, members_removed))
}
