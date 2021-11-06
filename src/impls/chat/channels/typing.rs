use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<TypingRequest>,
) -> ServerResult<Response<TypingResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let TypingRequest {
        guild_id,
        channel_id,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user_channel(guild_id, user_id, channel_id)?;
    chat_tree.check_perms(guild_id, Some(channel_id), user_id, "messages.send", false)?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::Typing(stream_event::Typing {
            user_id,
            guild_id,
            channel_id,
        }),
        Some(PermCheck::new(
            guild_id,
            Some(channel_id),
            "messages.view",
            false,
        )),
        EventContext::empty(),
    );

    Ok((TypingResponse {}).into_response())
}
