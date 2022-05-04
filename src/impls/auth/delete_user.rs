use super::*;

pub async fn logic(deps: &Dependencies, user_id: u64) -> ServerResult<()> {
    // delete from auth first
    let mut batch = Batch::default();
    batch.remove(user_id.to_be_bytes());
    batch.remove(token_key(user_id));
    batch.remove(atime_key(user_id));

    deps.auth_tree.apply_batch(batch).await?;

    // set profile to deleted
    deps.profile_tree
        .update_profile_logic(
            user_id,
            Some("Deleted User".to_string()),
            Some("".to_string()),
            Some(UserStatus::default()),
        )
        .await?;

    // remove metadata
    db::batch_delete_prefix(
        &deps.profile_tree.inner,
        db::profile::make_user_metadata_prefix(user_id),
    )
    .await?;

    // remove guild list
    db::batch_delete_prefix(
        &deps.chat_tree.chat_tree,
        db::chat::make_guild_list_key_prefix(user_id),
    )
    .await?;

    // end stream event
    let _ = deps.chat_event_canceller.send(user_id);

    Ok(())
}
