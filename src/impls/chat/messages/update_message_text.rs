use rkyv::de::deserializers::SharedDeserializeMap;

use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<UpdateMessageTextRequest>,
) -> ServerResult<Response<UpdateMessageTextResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let request = request.into_message().await?;

    let UpdateMessageTextRequest {
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

    if new_content.as_ref().map_or(true, |f| f.text.is_empty()) {
        return Err(ServerError::MessageContentCantBeEmpty.into());
    }

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

    let msg_content = if let Some(content) = &mut message.content {
        content
    } else {
        message.content = Some(Content::default());
        message.content.as_mut().unwrap()
    };
    msg_content.content = Some(content::Content::TextMessage(content::TextContent {
        content: new_content.clone(),
    }));

    let edited_at = get_time_secs();
    message.edited_at = Some(edited_at);

    let buf = rkyv_ser(&message);
    chat_tree.insert(key, buf).await?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::EditedMessage(Box::new(stream_event::MessageUpdated {
            guild_id,
            channel_id,
            message_id,
            edited_at,
            new_content,
        })),
        Some(PermCheck::new(
            guild_id,
            Some(channel_id),
            "messages.view",
            false,
        )),
        EventContext::empty(),
    );

    Ok((UpdateMessageTextResponse {}).into_response())
}
