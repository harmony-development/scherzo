use std::ops::Not;

use harmony_rust_sdk::api::emote::get_emote_pack_emotes_response::EmotePackEmotes;

use super::*;

pub async fn handler(
    svc: &EmoteServer,
    request: Request<GetEmotePackEmotesRequest>,
) -> ServerResult<Response<GetEmotePackEmotesResponse>> {
    svc.deps.auth(&request).await?;

    let GetEmotePackEmotesRequest { pack_id: pack_ids } = request.into_message().await?;

    let mut pack_emotes = HashMap::with_capacity(pack_ids.len());
    for pack_id in pack_ids {
        let pack_key = make_emote_pack_key(pack_id);

        if svc.deps.emote_tree.contains_key(pack_key).await?.not() {
            bail!(ServerError::EmotePackNotFound);
        }

        let emotes = svc
            .deps
            .emote_tree
            .inner
            .scan_prefix(&pack_key)
            .await
            .try_fold(Vec::new(), |mut all, res| {
                let (key, value) = res.map_err(ServerError::from)?;
                if key.len() > pack_key.len() {
                    all.push(db::deser_emote(value));
                }
                ServerResult::Ok(all)
            })?;

        pack_emotes.insert(pack_id, EmotePackEmotes::new(emotes));
    }

    Ok((GetEmotePackEmotesResponse::new(pack_emotes)).into_response())
}
