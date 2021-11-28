use super::*;

pub async fn handler(
    svc: &SyncServer,
    request: Request<PushRequest>,
) -> ServerResult<Response<PushResponse>> {
    let host = svc.auth(&request).await?;
    let key = make_host_key(&host);
    if !svc
        .deps
        .sync_tree
        .contains_key(&key)
        .map_err(ServerError::DbError)?
    {
        svc.deps
            .sync_tree
            .insert(&key, &[])
            .map_err(ServerError::DbError)?;
    }
    if let Some(event) = request.into_message().await?.event {
        svc.push_logic(&host, event)?;
    }
    Ok((PushResponse {}).into_response())
}
