use super::*;

pub async fn handler(
    svc: &EmoteServer,
    request: Request<DeleteEmotePackRequest>,
) -> ServerResult<Response<DeleteEmotePackResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let DeleteEmotePackRequest { pack_id } = request.into_message().await?;

    svc.deps
        .emote_tree
        .check_if_emote_pack_owner(pack_id, user_id)
        .await?;

    let key = make_emote_pack_key(pack_id);

    let mut batch = Batch::default();
    batch.remove(key);
    for res in svc.deps.emote_tree.scan_prefix(&key).await {
        let (key, _) = res?;
        batch.remove(key);
    }
    svc.deps
        .emote_tree
        .inner
        .apply_batch(batch)
        .await
        .map_err(ServerError::DbError)?;

    svc.deps
        .emote_tree
        .dequip_emote_pack_logic(user_id, pack_id)
        .await?;

    let equipped_users = svc
        .deps
        .emote_tree
        .calculate_users_pack_equipped(pack_id)
        .await?;
    svc.send_event_through_chan(
        EventSub::Homeserver,
        stream_event::Event::EmotePackDeleted(EmotePackDeleted { pack_id }),
        None,
        EventContext::new(equipped_users),
    );

    Ok((DeleteEmotePackResponse {}).into_response())
}
