use super::*;

pub async fn handler(
    svc: &mut EmoteServer,
    request: Request<EquipEmotePackRequest>,
) -> ServerResult<Response<EquipEmotePackResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let EquipEmotePackRequest { pack_id } = request.into_message().await?;

    let key = make_emote_pack_key(pack_id);
    if let Some(data) = svc.deps.emote_tree.get(key)? {
        let pack = db::deser_emote_pack(data);
        svc.deps
            .emote_tree
            .equip_emote_pack_logic(user_id, pack_id)?;
        svc.send_event_through_chan(
            EventSub::Homeserver,
            stream_event::Event::EmotePackAdded(EmotePackAdded { pack: Some(pack) }),
            None,
            EventContext::new(vec![user_id]),
        );
    } else {
        return Err(ServerError::EmotePackNotFound.into());
    }

    Ok((EquipEmotePackResponse {}).into_response())
}
