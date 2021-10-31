use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<PreviewGuildRequest>,
) -> ServerResult<Response<PreviewGuildResponse>> {
    let PreviewGuildRequest { invite_id } = request.into_message().await?;

    let key = make_invite_key(&invite_id);
    let guild_id = svc
        .chat_tree
        .chat_tree
        .get(&key)
        .map_err(ServerError::DbError)?
        .ok_or_else(|| ServerError::NoSuchInvite(invite_id.into()))
        .map(|raw| db::deser_invite_entry_guild_id(&raw))?;
    let guild = svc.chat_tree.get_guild_logic(guild_id)?;
    let member_count = svc
        .chat_tree
        .chat_tree
        .scan_prefix(&make_guild_mem_prefix(guild_id))
        .try_fold(0, |all, res| {
            if let Err(err) = res {
                return Err(ServerError::DbError(err).into());
            }
            ServerResult::Ok(all + 1)
        })?;

    Ok((PreviewGuildResponse {
        name: guild.name,
        picture: guild.picture,
        member_count,
    })
    .into_response())
}
