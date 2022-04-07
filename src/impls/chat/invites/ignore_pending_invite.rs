use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<IgnorePendingInviteRequest>,
) -> ServerResult<Response<IgnorePendingInviteResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let IgnorePendingInviteRequest {
        server_id,
        location,
    } = request.into_message().await?;

    let location = location.ok_or((
        "h.location-expected",
        "location not specified while it was expected",
    ))?;
    let location = match location {
        ignore_pending_invite_request::Location::ChannelId(channel_id) => {
            pending_invite::Location::ChannelId(channel_id)
        }
        ignore_pending_invite_request::Location::GuildInviteId(invite_id) => {
            pending_invite::Location::GuildInviteId(invite_id)
        }
    };

    svc.deps
        .chat_tree
        .remove_user_pending_invite(user_id, server_id.as_deref(), &location)
        .await?;

    Ok(IgnorePendingInviteResponse::new().into_response())
}
