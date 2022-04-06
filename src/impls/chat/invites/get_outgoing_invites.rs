use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetOutgoingInvitesRequest>,
) -> ServerResult<Response<GetOutgoingInvitesResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let outgoing = svc
        .deps
        .chat_tree
        .get(make_user_outgoing_invites_key(user_id))
        .await?
        .map_or_else(UserOutgoingInvites::default, db::deser_outgoing_invites);

    Ok(GetOutgoingInvitesResponse::new(outgoing.invites).into_response())
}
