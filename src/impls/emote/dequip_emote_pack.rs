use super::*;

pub async fn handler(
    svc: &EmoteServer,
    request: Request<DequipEmotePackRequest>,
) -> ServerResult<Response<DequipEmotePackResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let DequipEmotePackRequest { pack_id } = request.into_message().await?;

    svc.deps
        .emote_tree
        .dequip_emote_pack_logic(user_id, pack_id)
        .await?;

    svc.send_event_through_chan(
        EventSub::Homeserver,
        stream_event::Event::EmotePackDeleted(EmotePackDeleted { pack_id }),
        None,
        EventContext::new(vec![user_id]),
    );

    Ok((DequipEmotePackResponse {}).into_response())
}
