use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<DeleteOutgoingInviteRequest>,
) -> ServerResult<Response<DeleteOutgoingInviteResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let DeleteOutgoingInviteRequest { invite } = request.into_message().await?;

    let invite = invite.ok_or(("h.no-such-outgoing-invite", "no such outgoing invite found"))?;

    logic(svc.deps.as_ref(), user_id, &invite).await?;

    Ok(DeleteOutgoingInviteResponse::new().into_response())
}

pub async fn logic(deps: &Dependencies, user_id: u64, invite: &OutgoingInvite) -> ServerResult<()> {
    deps.chat_tree
        .remove_user_outgoing_invite(user_id, invite)
        .await?;
    Ok(())
}
