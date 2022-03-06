use rkyv::de::deserializers::SharedDeserializeMap;

use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<UpdateMessageContentRequest>,
) -> ServerResult<Response<UpdateMessageContentResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let request = request.into_message().await?;

    let UpdateMessageContentRequest {
        guild_id,
        channel_id,
        message_id,
        new_content,
    } = request;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree
        .check_guild_user_channel(guild_id, user_id, channel_id)
        .await?;
    chat_tree
        .check_perms(guild_id, Some(channel_id), user_id, "messages.send", false)
        .await?;

    let new_content = chat_tree
        .process_message_content(&svc.deps, new_content)
        .await?;

    let key = make_msg_key(guild_id, channel_id, message_id);
    let Some(message_raw) = chat_tree.get(key).await? else {
        bail!(ServerError::NoSuchMessage { guild_id, channel_id, message_id });
    };
    let message_archived = rkyv_arch::<Message>(&message_raw);

    if message_archived.author_id != user_id {
        bail!((
            "h.not-author",
            "you must be the author of a message to edit it"
        ));
    }

    let mut message: Message = message_archived
        .deserialize(&mut SharedDeserializeMap::default())
        .unwrap();

    let edited_at = get_time_secs();
    message.content = Some(new_content.clone());
    message.edited_at = Some(edited_at);

    let buf = rkyv_ser(&message);
    chat_tree.insert(key, buf).await?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::EditedMessage(stream_event::MessageUpdated {
            guild_id,
            channel_id,
            message_id,
            edited_at,
            new_content: Some(new_content),
        }),
        Some(PermCheck::new(
            guild_id,
            Some(channel_id),
            "messages.view",
            false,
        )),
        EventContext::empty(),
    );

    Ok(UpdateMessageContentResponse::new().into_response())
}
