use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<PreviewGuildRequest>,
) -> ServerResult<Response<PreviewGuildResponse>> {
    let PreviewGuildRequest { invite_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    let key = make_invite_key(&invite_id);
    let guild_id = chat_tree
        .get(&key)
        .await?
        .ok_or_else(|| ServerError::NoSuchInvite(invite_id.into()))
        .map(|raw| db::deser_invite_entry_guild_id(&raw))?;
    let guild = chat_tree.get_guild_logic(guild_id).await?;
    // TODO(yusdacra): don't count members by iterating through scan prefix result
    // instead keep a member count entry in db
    let member_count = chat_tree
        .scan_prefix(&make_guild_mem_prefix(guild_id))
        .await
        .try_fold(0, |all, res| res.map(|_| all + 1))?;

    Ok((PreviewGuildResponse {
        name: guild.name,
        picture: guild.picture,
        member_count,
    })
    .into_response())
}
