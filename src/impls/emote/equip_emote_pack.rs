use super::*;

pub async fn handler(
    svc: &EmoteServer,
    request: Request<EquipEmotePackRequest>,
) -> ServerResult<Response<EquipEmotePackResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let EquipEmotePackRequest { pack_id } = request.into_message().await?;

    let key = make_emote_pack_key(pack_id);
    if let Some(data) = svc.deps.emote_tree.get(key).await? {
        let pack = db::deser_emote_pack(data);
        svc.deps
            .emote_tree
            .equip_emote_pack_logic(user_id, pack_id)
            .await?;
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
