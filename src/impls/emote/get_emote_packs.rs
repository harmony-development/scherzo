use super::*;

pub async fn handler(
    svc: &EmoteServer,
    request: Request<GetEmotePacksRequest>,
) -> ServerResult<Response<GetEmotePacksResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let prefix = make_equipped_emote_prefix(user_id);
    let equipped_packs =
        svc.deps
            .emote_tree
            .inner
            .scan_prefix(&prefix)
            .try_fold(Vec::new(), |mut all, res| {
                let (key, _) = res.map_err(ServerError::from)?;
                if key.len() == make_equipped_emote_key(user_id, 0).len() {
                    let pack_id =
                    // Safety: since it will always be 8 bytes left afterwards
                    u64::from_be_bytes(unsafe {
                        key.split_at(prefix.len()).1.try_into().unwrap_unchecked()
                    });
                    all.push(pack_id);
                }
                ServerResult::Ok(all)
            })?;

    let packs = svc
        .deps
        .emote_tree
        .inner
        .scan_prefix(EMOTEPACK_PREFIX)
        .try_fold(Vec::new(), |mut all, res| {
            let (key, val) = res.map_err(ServerError::from)?;
            if key.len() == make_emote_pack_key(0).len() {
                let pack_id =
                    // Safety: since it will always be 8 bytes left afterwards
                    u64::from_be_bytes(unsafe {
                        key.split_at(EMOTEPACK_PREFIX.len())
                            .1
                            .try_into()
                            .unwrap_unchecked()
                    });
                if equipped_packs.contains(&pack_id) {
                    all.push(db::deser_emote_pack(val));
                }
            }
            ServerResult::Ok(all)
        })?;

    Ok((GetEmotePacksResponse { packs }).into_response())
}
