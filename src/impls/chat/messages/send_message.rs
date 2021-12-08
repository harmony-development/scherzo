use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<SendMessageRequest>,
) -> ServerResult<Response<SendMessageResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let mut request = request.into_message().await?;
    let guild_id = request.guild_id;
    let channel_id = request.channel_id;
    let echo_id = request.echo_id;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree
        .check_guild_user_channel(guild_id, user_id, channel_id)
        .await?;
    chat_tree
        .check_perms(guild_id, Some(channel_id), user_id, "messages.send", false)
        .await?;

    let content = chat_tree
        .process_message_content(
            request.content.take(),
            svc.deps.config.media.media_root.as_path(),
            &svc.deps.config.host,
        )
        .await?;
    request.content = Some(content);
    let (message_id, message) = chat_tree.send_message_logic(user_id, request).await?;

    let is_cmd_channel = chat_tree
        .admin_guild_keys
        .get()
        .map_or(false, |keys| keys.check_if_cmd(guild_id, channel_id));

    let action_content = if is_cmd_channel {
        if let Some(content::Content::TextMessage(content::TextContent {
            content: Some(FormattedText { text, .. }),
        })) = message.content.as_ref().and_then(|c| c.content.as_ref())
        {
            svc.deps.action_processor.run(text).await.ok()
        } else {
            None
        }
    } else {
        None
    };

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::SentMessage(Box::new(stream_event::MessageSent {
            echo_id,
            guild_id,
            channel_id,
            message_id,
            message: Some(message),
        })),
        Some(PermCheck::new(
            guild_id,
            Some(channel_id),
            "messages.view",
            false,
        )),
        EventContext::empty(),
    );

    if let Some(msg) = action_content {
        let content = content::Content::TextMessage(content::TextContent {
            content: Some(FormattedText::new(msg, Vec::new())),
        });
        let (message_id, message) = chat_tree
            .send_with_system(guild_id, channel_id, content)
            .await?;
        svc.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::SentMessage(Box::new(stream_event::MessageSent {
                echo_id,
                guild_id,
                channel_id,
                message_id,
                message: Some(message),
            })),
            Some(PermCheck::new(
                guild_id,
                Some(channel_id),
                "messages.view",
                false,
            )),
            EventContext::empty(),
        );
    }

    Ok((SendMessageResponse { message_id }).into_response())
}
