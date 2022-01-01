use super::*;

pub async fn handler(
    svc: &EmoteServer,
    request: Request<CreateEmotePackRequest>,
) -> ServerResult<Response<CreateEmotePackResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let CreateEmotePackRequest { pack_name } = request.into_message().await?;

    let pack_id = gen_rand_u64();
    let key = make_emote_pack_key(pack_id);

    let emote_pack = EmotePack {
        pack_id,
        pack_name,
        pack_owner: user_id,
    };
    let data = rkyv_ser(&emote_pack);

    svc.deps.emote_tree.insert(key, data).await?;

    svc.deps
        .emote_tree
        .equip_emote_pack_logic(user_id, pack_id)
        .await?;
    svc.send_event_through_chan(
        EventSub::Homeserver,
        stream_event::Event::EmotePackAdded(EmotePackAdded {
            pack: Some(emote_pack),
        }),
        None,
        EventContext::new(vec![user_id]),
    );

    Ok((CreateEmotePackResponse { pack_id }).into_response())
}
