use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<UpdatePrivateChannelNameRequest>,
) -> ServerResult<Response<UpdatePrivateChannelNameResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let UpdatePrivateChannelNameRequest {
        channel_id,
        name: new_name,
    } = request.into_message().await?;

    svc.deps
        .chat_tree
        .check_private_channel_user(channel_id, user_id)
        .await?;

    logic(svc.deps.as_ref(), channel_id, new_name).await?;

    Ok(UpdatePrivateChannelNameResponse::new().into_response())
}

pub async fn logic(deps: &Dependencies, channel_id: u64, new_name: String) -> ServerResult<()> {
    let mut private_channel = get_private_channel::logic(deps, channel_id).await?;
    private_channel.name = new_name.is_empty().not().then(|| new_name);

    deps.chat_tree
        .insert(make_pc_key(channel_id), rkyv_ser(&private_channel))
        .await?;

    Ok(())
}
