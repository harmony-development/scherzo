use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<IgnorePendingInviteRequest>,
) -> ServerResult<Response<IgnorePendingInviteResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let IgnorePendingInviteRequest { invite } = request.into_message().await?;

    let invite = invite.ok_or((
        "h.invite-expected",
        "no invite specified while one was expected",
    ))?;

    svc.deps
        .chat_tree
        .remove_user_pending_invite(user_id, &invite)
        .await?;

    Ok(IgnorePendingInviteResponse::new().into_response())
}
