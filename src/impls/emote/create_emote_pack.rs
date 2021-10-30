use super::*;

pub async fn handler(
    svc: &mut EmoteServer,
    request: Request<CreateEmotePackRequest>,
) -> ServerResult<Response<CreateEmotePackResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let CreateEmotePackRequest { pack_name } = request.into_message().await?;

    let pack_id = gen_rand_u64();
    let key = make_emote_pack_key(pack_id);

    let emote_pack = EmotePack {
        pack_id,
        pack_name,
        pack_owner: user_id,
    };
    let data = rkyv_ser(&emote_pack);

    svc.emote_tree.insert(key, data)?;

    svc.emote_tree.equip_emote_pack_logic(user_id, pack_id)?;
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
