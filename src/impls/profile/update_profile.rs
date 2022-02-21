use super::*;

pub async fn handler(
    svc: &ProfileServer,
    request: Request<UpdateProfileRequest>,
) -> ServerResult<Response<UpdateProfileResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.deps.auth(&request).await?;

    let UpdateProfileRequest {
        new_user_name,
        new_user_avatar,
        new_user_status,
    } = request.into_message().await?;

    if let Some(username) = new_user_name.as_deref() {
        if svc.deps.profile_tree.does_username_exist(username).await? {
            bail!(ServerError::UserAlreadyExists);
        }
    }

    svc.deps
        .profile_tree
        .update_profile_logic(
            user_id,
            new_user_name.clone(),
            new_user_avatar.clone(),
            new_user_status,
        )
        .await?;

    svc.send_event_through_chan(
        EventSub::Homeserver,
        stream_event::Event::ProfileUpdated(ProfileUpdated {
            user_id,
            new_username: new_user_name,
            new_avatar: new_user_avatar,
            new_status: new_user_status,
            new_account_kind: None,
        }),
        None,
        EventContext::new(
            svc.deps
                .chat_tree
                .calculate_users_seeing_user(user_id)
                .await?,
        ),
    );

    Ok((UpdateProfileResponse {}).into_response())
}
