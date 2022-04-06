use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetPendingInvitesRequest>,
) -> ServerResult<Response<GetPendingInvitesResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let pending = svc
        .deps
        .chat_tree
        .get(make_user_pending_invites_key(user_id))
        .await?
        .map_or_else(UserPendingInvites::default, db::deser_pending_invites);

    Ok(GetPendingInvitesResponse::new(pending.invites).into_response())
}
