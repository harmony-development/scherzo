use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<GetBannedUsersRequest>,
) -> ServerResult<Response<GetBannedUsersResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let GetBannedUsersRequest { guild_id } = request.into_message().await?;

    let prefix = make_guild_banned_mem_prefix(guild_id);
    let banned_users =
        svc.chat_tree
            .chat_tree
            .scan_prefix(&prefix)
            .try_fold(Vec::new(), |mut all, res| {
                let (key, _) = res.map_err(ServerError::from)?;
                if key.len() == make_banned_member_key(0, 0).len() {
                    all.push(u64::from_be_bytes(unsafe {
                        key.split_at(prefix.len()).1.try_into().unwrap_unchecked()
                    }));
                }
                ServerResult::Ok(all)
            })?;

    Ok((GetBannedUsersResponse { banned_users }).into_response())
}
