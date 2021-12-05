use super::*;

pub async fn handler(
    svc: &EmoteServer,
    request: Request<GetEmotePackEmotesRequest>,
) -> ServerResult<Response<GetEmotePackEmotesResponse>> {
    svc.deps.valid_sessions.auth(&request)?;

    let GetEmotePackEmotesRequest { pack_id } = request.into_message().await?;

    let pack_key = make_emote_pack_key(pack_id);

    if svc.deps.emote_tree.get(pack_key).await?.is_none() {
        return Err(ServerError::EmotePackNotFound.into());
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

    Ok((GetEmotePackEmotesResponse { emotes }).into_response())
}
