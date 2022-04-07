use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<DeleteOutgoingInviteRequest>,
) -> ServerResult<Response<DeleteOutgoingInviteResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let DeleteOutgoingInviteRequest { invite } = request.into_message().await?;

    let invite = invite.ok_or(("h.invite-expected", "an outgoing invite must be specified"))?;

    svc.deps
        .chat_tree
        .remove_user_outgoing_invite(user_id, &invite)
        .await?;

    Ok(DeleteOutgoingInviteResponse::new().into_response())
}
