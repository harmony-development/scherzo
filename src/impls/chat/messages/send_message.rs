use hrpc::exports::futures_util::FutureExt;

use crate::impls::admin_action;

use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<SendMessageRequest>,
) -> ServerResult<Response<SendMessageResponse>> {
    let user_id = svc.deps.auth(&request).await?;

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

    chat_tree.process_message_overrides(request.overrides.as_ref())?;
    let content = chat_tree
        .process_message_content(svc.deps.as_ref(), request.content.take())
        .await?;

    let admin_action = chat_tree
        .admin_guild_keys
        .get()
        .map_or(false, |keys| keys.check_if_cmd(guild_id, channel_id))
        .then(|| content.text.parse::<admin_action::AdminAction>().ok())
        .flatten();

    let (message_id, message) = chat_tree
        .send_message_logic(
            guild_id,
            channel_id,
            user_id,
            content,
            request.overrides,
            request.in_reply_to,
            request.metadata,
        )
        .await?;

    let action_content = opt_fut(admin_action.map(|action| {
        admin_action::run(svc.deps.as_ref(), action)
            .map(|err| err.unwrap_or_else(|err| format!("error: {err}")))
    }))
    .await;

    svc.broadcast(
        EventSub::Guild(guild_id),
        stream_event::Event::SentMessage(stream_event::MessageSent {
            echo_id,
            guild_id,
            channel_id,
            message_id,
            message: Some(message),
        }),
        Some(PermCheck::new(
            guild_id,
            Some(channel_id),
            "messages.view",
            false,
        )),
        EventContext::empty(),
    );

    if let Some(msg) = action_content {
        let content = Content::default().with_text(msg);
        let (message_id, message) = chat_tree
            .send_with_system(guild_id, channel_id, content)
            .await?;
        svc.broadcast(
            EventSub::Guild(guild_id),
            stream_event::Event::SentMessage(stream_event::MessageSent {
                echo_id,
                guild_id,
                channel_id,
                message_id,
                message: Some(message),
            }),
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
