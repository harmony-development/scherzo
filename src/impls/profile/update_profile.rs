use super::*;

pub async fn handler(
    svc: &ProfileServer,
    request: Request<UpdateProfileRequest>,
) -> ServerResult<Response<UpdateProfileResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let UpdateProfileRequest {
        new_user_name,
        new_user_avatar,
        new_user_status,
        new_is_bot,
    } = request.into_message().await?;

    svc.deps.profile_tree.update_profile_logic(
        user_id,
        new_user_name.clone(),
        new_user_avatar.clone(),
        new_user_status,
        new_is_bot,
    )?;

    svc.send_event_through_chan(
        EventSub::Homeserver,
        stream_event::Event::ProfileUpdated(ProfileUpdated {
            user_id,
            new_username: new_user_name,
            new_avatar: new_user_avatar,
            new_status: new_user_status,
            new_is_bot,
        }),
        None,
        EventContext::new(svc.deps.chat_tree.calculate_users_seeing_user(user_id)?),
    );

    Ok((UpdateProfileResponse {}).into_response())
}
