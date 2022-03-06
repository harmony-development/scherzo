use super::*;

pub async fn handler(
    svc: &ProfileServer,
    request: Request<UpdateStatusRequest>,
) -> ServerResult<Response<UpdateStatusResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.deps.auth(&request).await?;

    let UpdateStatusRequest { new_status } = request.into_message().await?;

    svc.deps
        .profile_tree
        .update_profile_logic(user_id, None, None, new_status.clone())
        .await?;

    svc.send_event_through_chan(
        EventSub::Homeserver,
        stream_event::Event::StatusUpdated(StatusUpdated {
            user_id,
            new_status,
        }),
        None,
        EventContext::new(
            svc.deps
                .chat_tree
                .calculate_users_seeing_user(user_id)
                .await?,
        ),
    );

    Ok(UpdateStatusResponse::new().into_response())
}
