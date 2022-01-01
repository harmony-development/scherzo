use super::*;

pub async fn handler(
    svc: &EmoteServer,
    request: Request<GetEmotePacksRequest>,
) -> ServerResult<Response<GetEmotePacksResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let prefix = make_equipped_emote_prefix(user_id);
    let equipped_packs = svc
        .deps
        .emote_tree
        .inner
        .scan_prefix(&prefix)
        .await
        .try_fold(Vec::new(), |mut all, res| {
            let (key, _) = res.map_err(ServerError::from)?;
            if key.len() == make_equipped_emote_key(user_id, 0).len() {
                // Safety: since it will always be 8 bytes left afterwards
                let pack_id = deser_id(key.split_at(prefix.len()).1);
                all.push(pack_id);
            }
            ServerResult::Ok(all)
        })?;

    let packs = svc
        .deps
        .emote_tree
        .inner
        .scan_prefix(EMOTEPACK_PREFIX)
        .await
        .try_fold(Vec::new(), |mut all, res| {
            let (key, val) = res.map_err(ServerError::from)?;
            if key.len() == make_emote_pack_key(0).len() {
                // Safety: since it will always be 8 bytes left afterwards
                let pack_id = deser_id(key.split_at(EMOTEPACK_PREFIX.len()).1);
                if equipped_packs.contains(&pack_id) {
                    all.push(db::deser_emote_pack(val));
                }
            }
            ServerResult::Ok(all)
        })?;

    Ok((GetEmotePacksResponse { packs }).into_response())
}
