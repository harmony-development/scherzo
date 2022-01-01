use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetGuildListRequest>,
) -> ServerResult<Response<GetGuildListResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let prefix = make_guild_list_key_prefix(user_id);
    let guilds =
        svc.deps
            .chat_tree
            .scan_prefix(&prefix)
            .await
            .try_fold(Vec::new(), |mut all, res| {
                let (guild_id_raw, _) = res?;
                let (id_raw, host_raw) = guild_id_raw
                    .split_at(prefix.len())
                    .1
                    .split_at(size_of::<u64>());

                // Safety: this unwrap can never cause UB since we split at u64 boundary
                let guild_id = deser_id(id_raw);
                // Safety: we never store non UTF-8 hosts, so this can't cause UB
                let host = unsafe { std::str::from_utf8_unchecked(host_raw) };

                all.push(GuildListEntry {
                    guild_id,
                    server_id: host.to_string(),
                });

                ServerResult::Ok(all)
            })?;

    Ok((GetGuildListResponse { guilds }).into_response())
}
