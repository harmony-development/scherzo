use super::*;

pub async fn handler(
    svc: &mut EmoteServer,
    request: Request<DeleteEmotePackRequest>,
) -> ServerResult<Response<DeleteEmotePackResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let DeleteEmotePackRequest { pack_id } = request.into_message().await?;

    svc.emote_tree.check_if_emote_pack_owner(pack_id, user_id)?;

    let key = make_emote_pack_key(pack_id);

    let mut batch = Batch::default();
    batch.remove(key);
    for res in svc.emote_tree.inner.scan_prefix(&key) {
        let (key, _) = res.map_err(ServerError::DbError)?;
        batch.remove(key);
    }
    svc.emote_tree
        .inner
        .apply_batch(batch)
        .map_err(ServerError::DbError)?;

    svc.emote_tree.dequip_emote_pack_logic(user_id, pack_id)?;

    let equipped_users = svc.emote_tree.calculate_users_pack_equipped(pack_id)?;
    svc.send_event_through_chan(
        EventSub::Homeserver,
        stream_event::Event::EmotePackDeleted(EmotePackDeleted { pack_id }),
        None,
        EventContext::new(equipped_users),
    );

    Ok((DeleteEmotePackResponse {}).into_response())
}
