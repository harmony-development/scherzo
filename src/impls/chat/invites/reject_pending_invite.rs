use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<RejectPendingInviteRequest>,
) -> ServerResult<Response<RejectPendingInviteResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let RejectPendingInviteRequest { invite } = request.into_message().await?;

    let invite = invite.ok_or(("h.no-invite", "expected pending invite to be specified"))?;

    svc.deps
        .chat_tree
        .remove_user_pending_invite(user_id, &invite)
        .await?;

    let location = match invite.location.ok_or("expected location")? {
        pending_invite::Location::ChannelId(channel_id) => {
            outgoing_invite::Location::ChannelId(channel_id)
        }
        pending_invite::Location::GuildInviteId(invite_id) => {
            outgoing_invite::Location::GuildInviteId(invite_id)
        }
    };
    svc.dispatch_user_invite_rejected(invite.inviter_id, user_id, location)
        .await?;

    Ok(RejectPendingInviteResponse::new().into_response())
}
