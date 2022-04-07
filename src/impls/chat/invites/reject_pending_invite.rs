use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<RejectPendingInviteRequest>,
) -> ServerResult<Response<RejectPendingInviteResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let RejectPendingInviteRequest {
        server_id,
        location,
    } = request.into_message().await?;

    let location = location.ok_or((
        "h.location-expected",
        "location not specified while it was expected",
    ))?;
    let location = match location {
        reject_pending_invite_request::Location::ChannelId(channel_id) => {
            pending_invite::Location::ChannelId(channel_id)
        }
        reject_pending_invite_request::Location::GuildInviteId(invite_id) => {
            pending_invite::Location::GuildInviteId(invite_id)
        }
    };

    let invite = svc
        .deps
        .chat_tree
        .remove_user_pending_invite(user_id, server_id.as_deref(), &location)
        .await?;

    let location = match location {
        pending_invite::Location::ChannelId(channel_id) => {
            user_rejected_invite::Location::ChannelId(channel_id)
        }
        pending_invite::Location::GuildInviteId(invite_id) => {
            user_rejected_invite::Location::GuildInviteId(invite_id)
        }
    };
    svc.dispatch_user_invite_rejected(invite.inviter_id, user_id, location)
        .await?;

    Ok(RejectPendingInviteResponse::new().into_response())
}
