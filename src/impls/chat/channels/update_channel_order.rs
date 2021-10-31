use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<UpdateChannelOrderRequest>,
) -> ServerResult<Response<UpdateChannelOrderResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let UpdateChannelOrderRequest {
        guild_id,
        channel_id,
        new_position,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user_channel(guild_id, user_id, channel_id)?;
    chat_tree.check_perms(
        guild_id,
        Some(channel_id),
        user_id,
        "channels.manage.move",
        false,
    )?;

    if let Some(position) = new_position {
        chat_tree.update_channel_order_logic(guild_id, channel_id, Some(position.clone()))?;

        svc.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::EditedChannelPosition(stream_event::ChannelPositionUpdated {
                guild_id,
                channel_id,
                new_position: Some(position),
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

    Ok((UpdateChannelOrderResponse {}).into_response())
}
