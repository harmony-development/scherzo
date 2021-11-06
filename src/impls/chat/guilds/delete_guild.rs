use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<DeleteGuildRequest>,
) -> ServerResult<Response<DeleteGuildResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let DeleteGuildRequest { guild_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id)?;
    chat_tree.check_perms(guild_id, None, user_id, "guild.manage.delete", false)?;

    let guild_members = chat_tree.get_guild_members_logic(guild_id)?.members;

    let guild_data =
        chat_tree
            .scan_prefix(&guild_id.to_be_bytes())
            .try_fold(Vec::new(), |mut all, res| {
                all.push(res?.0);
                ServerResult::Ok(all)
            })?;

    let mut batch = Batch::default();
    for key in guild_data {
        batch.remove(key);
    }
    chat_tree
        .chat_tree
        .apply_batch(batch)
        .map_err(ServerError::DbError)?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::DeletedGuild(stream_event::GuildDeleted { guild_id }),
        None,
        EventContext::empty(),
    );

    let mut local_ids = Vec::new();
    for member_id in guild_members {
        match svc.deps.profile_tree.local_to_foreign_id(member_id)? {
            Some((foreign_id, target)) => svc.dispatch_event(
                target,
                DispatchKind::UserRemovedFromGuild(SyncUserRemovedFromGuild {
                    user_id: foreign_id,
                    guild_id,
                }),
            ),
            None => {
                chat_tree.remove_guild_from_guild_list(user_id, guild_id, "")?;
                local_ids.push(member_id);
            }
        }
    }
    svc.send_event_through_chan(
        EventSub::Homeserver,
        stream_event::Event::GuildRemovedFromList(stream_event::GuildRemovedFromList {
            guild_id,
            homeserver: String::new(),
        }),
        None,
        EventContext::new(local_ids),
    );

    Ok((DeleteGuildResponse {}).into_response())
}
