use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetGuildListRequest>,
) -> ServerResult<Response<GetGuildListResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let guilds = svc.deps.chat_tree.get_user_guilds(user_id).await?;

    Ok((GetGuildListResponse { guilds }).into_response())
}
