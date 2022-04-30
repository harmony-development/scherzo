use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<UpdatePrivateChannelRequest>,
) -> ServerResult<Response<UpdatePrivateChannelResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let UpdatePrivateChannelRequest {
        channel_id,
        new_name,
        added_members,
        removed_members,
    } = request.into_message().await?;

    svc.deps
        .chat_tree
        .check_private_channel_user(channel_id, user_id)
        .await?;

    if let Some(new_name) = new_name {
        update_private_channel_name::logic(svc.deps.as_ref(), channel_id, new_name.clone()).await?;

        broadcast!(
            svc,
            EventSub::PrivateChannel(channel_id),
            PrivateChannelUpdated {
                channel_id,
                new_name: Some(new_name),
            }
        );
    }

    if added_members.is_empty().not() || removed_members.is_empty().not() {
        let (added, removed) =
            update_private_channel_members::logic(svc, channel_id, added_members, removed_members)
                .await?;

        // dispatch invites to new members
        for invitee_id in added {
            svc.dispatch_user_invite_received(
                user_id,
                invitee_id,
                pending_invite::Location::ChannelId(channel_id),
            )
            .await?;
        }

        // dispatch left events to removed users
        for member in removed {
            svc.dispatch_private_channel_leave(channel_id, member)
                .await?;
            broadcast!(
                svc,
                EventSub::PrivateChannel(channel_id),
                UserLeftPrivateChannel {
                    channel_id,
                    user_id,
                }
            );
        }
    }

    Ok(UpdatePrivateChannelResponse::new().into_response())
}
