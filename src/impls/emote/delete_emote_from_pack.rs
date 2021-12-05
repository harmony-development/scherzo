use super::*;

pub async fn handler(
    svc: &EmoteServer,
    request: Request<DeleteEmoteFromPackRequest>,
) -> ServerResult<Response<DeleteEmoteFromPackResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let DeleteEmoteFromPackRequest { pack_id, name } = request.into_message().await?;

    svc.deps
        .emote_tree
        .check_if_emote_pack_owner(pack_id, user_id)
        .await?;

    let key = make_emote_pack_emote_key(pack_id, &name);

    svc.deps.emote_tree.remove(key).await?;

    let equipped_users = svc
        .deps
        .emote_tree
        .calculate_users_pack_equipped(pack_id)
        .await?;
    svc.send_event_through_chan(
        EventSub::Homeserver,
        stream_event::Event::EmotePackEmotesUpdated(EmotePackEmotesUpdated {
            pack_id,
            added_emotes: Vec::new(),
            deleted_emotes: vec![name],
        }),
        None,
        EventContext::new(equipped_users),
    );

    Ok((DeleteEmoteFromPackResponse {}).into_response())
}
