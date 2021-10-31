use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<SendMessageRequest>,
) -> ServerResult<Response<SendMessageResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let mut request = request.into_message().await?;
    let guild_id = request.guild_id;
    let channel_id = request.channel_id;
    let echo_id = request.echo_id;

    svc.chat_tree
        .check_guild_user_channel(guild_id, user_id, channel_id)?;
    svc.chat_tree
        .check_perms(guild_id, Some(channel_id), user_id, "messages.send", false)?;

    let content = svc
        .chat_tree
        .process_message_content(
            request.content.take(),
            svc.media_root.as_path(),
            &svc.host,
        )
        .await?;
    request.content = Some(content);
    let (message_id, message) = svc.chat_tree.send_message_logic(user_id, request)?;

    let is_cmd_channel = svc
        .chat_tree
        .get_admin_guild_keys()?
        .map(|(g, _, c)| (g, c))
        == Some((guild_id, channel_id));

    let action_content = if is_cmd_channel {
        if let Some(content::Content::TextMessage(content::TextContent {
            content: Some(FormattedText { text, .. }),
        })) = message.content.as_ref().and_then(|c| c.content.as_ref())
        {
            svc.action_processor.run(text).ok()
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
        let content = Content {
            content: Some(content::Content::TextMessage(content::TextContent {
                content: Some(FormattedText::new(msg, Vec::new())),
            })),
        };
        let (message_id, message) = svc
            .chat_tree
            .send_with_system(guild_id, channel_id, content)?;
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
