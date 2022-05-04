use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<TypingRequest>,
) -> ServerResult<Response<TypingResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let TypingRequest {
        guild_id,
        channel_id,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree
        .check_channel_user(guild_id, user_id, channel_id)
        .await?;
    chat_tree
        .check_perms(guild_id, Some(channel_id), user_id, "messages.send", false)
        .await?;

    svc.broadcast(
        guild_id.map_or(EventSub::PrivateChannel(channel_id), EventSub::Guild),
        stream_event::Event::Typing(stream_event::Typing {
            user_id,
            guild_id,
            channel_id,
        }),
        PermCheck::maybe_new(guild_id, channel_id, "messages.view"),
        EventContext::empty(),
    );

    Ok(TypingResponse::new().into_response())
}
