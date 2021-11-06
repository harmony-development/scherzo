use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<UpdateGuildInformationRequest>,
) -> ServerResult<Response<UpdateGuildInformationResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let UpdateGuildInformationRequest {
        guild_id,
        new_name,
        new_picture,
        new_metadata,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id)?;

    let mut guild_info = chat_tree.get_guild_logic(guild_id)?;

    chat_tree.check_perms(
        guild_id,
        None,
        user_id,
        "guild.manage.change-information",
        false,
    )?;

    if let Some(new_name) = new_name.clone() {
        guild_info.name = new_name;
    }
    if let Some(new_picture) = new_picture.clone() {
        guild_info.picture = Some(new_picture);
    }
    if let Some(new_metadata) = new_metadata.clone() {
        guild_info.metadata = Some(new_metadata);
    }

    chat_tree.put_guild_logic(guild_id, guild_info)?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::EditedGuild(stream_event::GuildUpdated {
            guild_id,
            new_name,
            new_picture,
            new_metadata,
        }),
        None,
        EventContext::empty(),
    );

    Ok((UpdateGuildInformationResponse {}).into_response())
}
