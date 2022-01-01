use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetGuildMembersRequest>,
) -> ServerResult<Response<GetGuildMembersResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let GetGuildMembersRequest { guild_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id).await?;

    chat_tree
        .get_guild_members_logic(guild_id)
        .await
        .map(IntoResponse::into_response)
}
