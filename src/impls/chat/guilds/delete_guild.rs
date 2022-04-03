use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<DeleteGuildRequest>,
) -> ServerResult<Response<DeleteGuildResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let DeleteGuildRequest { guild_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;
    chat_tree
        .check_perms(guild_id, None, user_id, "guild.manage.delete", false)
        .await?;

    let guild_members = chat_tree.get_guild_members_logic(guild_id).await?.members;

    let guild_data = chat_tree
        .scan_prefix(&guild_id.to_be_bytes())
        .await
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
        .await
        .map_err(ServerError::DbError)?;

    svc.broadcast(
        EventSub::Guild(guild_id),
        stream_event::Event::DeletedGuild(stream_event::GuildDeleted { guild_id }),
        None,
        EventContext::empty(),
    );

    let mut local_ids = Vec::new();
    for member_id in guild_members {
        match svc.deps.profile_tree.local_to_foreign_id(member_id).await? {
            Some((foreign_id, target)) => svc.dispatch_event(
                target,
                DispatchKind::UserRemovedFromGuild(SyncUserRemovedFromGuild {
                    user_id: foreign_id,
                    guild_id,
                }),
            ),
            None => {
                chat_tree
                    .remove_guild_from_guild_list(user_id, guild_id, "")
                    .await?;
                local_ids.push(member_id);
            }
        }
    }
    svc.broadcast(
        EventSub::Homeserver,
        stream_event::Event::GuildRemovedFromList(stream_event::GuildRemovedFromList {
            guild_id,
            server_id: String::new(),
        }),
        None,
        EventContext::new(local_ids),
    );

    Ok(DeleteGuildResponse::new().into_response())
}
