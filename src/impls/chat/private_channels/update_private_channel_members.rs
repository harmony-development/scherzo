use super::*;

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
