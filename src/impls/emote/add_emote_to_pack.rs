use super::*;

pub async fn handler(
    svc: &EmoteServer,
    request: Request<AddEmoteToPackRequest>,
) -> ServerResult<Response<AddEmoteToPackResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let AddEmoteToPackRequest {
        pack_id,
        image_id,
        name,
    } = request.into_message().await?;

    svc.deps
        .emote_tree
        .check_if_emote_pack_owner(pack_id, user_id)
        .await?;

    let emote = Emote::new(image_id, name);

    let emote_key = make_emote_pack_emote_key(pack_id, &emote.name);
    let data = rkyv_ser(&emote);

    svc.deps.emote_tree.insert(emote_key, data).await?;

    let equipped_users = svc
        .deps
        .emote_tree
        .calculate_users_pack_equipped(pack_id)
        .await?;
    svc.send_event_through_chan(
        EventSub::Homeserver,
        stream_event::Event::EmotePackEmotesUpdated(EmotePackEmotesUpdated {
            pack_id,
            added_emotes: vec![emote],
            deleted_emotes: Vec::new(),
        }),
        None,
        EventContext::new(equipped_users),
    );

    Ok(AddEmoteToPackResponse::new().into_response())
}
