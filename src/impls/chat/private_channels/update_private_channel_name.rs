use super::*;

pub async fn logic(deps: &Dependencies, channel_id: u64, new_name: String) -> ServerResult<()> {
    let mut private_channel = get_private_channel::logic(deps, channel_id).await?;
    private_channel.name = new_name.is_empty().not().then(|| new_name);

    deps.chat_tree
        .insert(make_pc_key(channel_id), rkyv_ser(&private_channel))
        .await?;

    Ok(())
}
