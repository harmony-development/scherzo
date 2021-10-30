use super::*;

pub async fn handler(
    svc: &mut AuthServer,
    request: Request<LoginFederatedRequest>,
) -> Result<Response<LoginFederatedResponse>, HrpcServerError> {
    let LoginFederatedRequest {
        auth_token,
        server_id,
    } = request.into_message().await?;

    svc.is_host_allowed(&server_id)?;

    if let Some(token) = auth_token {
        let keys_manager = svc.keys_manager()?;
        let pubkey = keys_manager.get_key(server_id.into()).await?;
        keys::verify_token(&token, &pubkey)?;
        let TokenData {
            user_id: foreign_id,
            server_id,
            username,
            avatar,
        } = TokenData::decode(token.data.as_slice()).map_err(|_| ServerError::InvalidTokenData)?;

        let local_user_id = if let Some(id) = svc
            .profile_tree
            .foreign_to_local_id(foreign_id, &server_id)?
        {
            id
        } else {
            let local_id = gen_rand_u64();

            let mut batch = Batch::default();
            // Add the local to foreign user key entry
            batch.insert(
                make_local_to_foreign_user_key(local_id).to_vec(),
                [&foreign_id.to_be_bytes(), server_id.as_bytes()].concat(),
            );
            // Add the foreign to local user key entry
            batch.insert(
                make_foreign_to_local_user_key(foreign_id, &server_id),
                local_id.to_be_bytes().to_vec(),
            );
            // Add the profile entry
            let profile = Profile {
                is_bot: false,
                user_status: UserStatus::OfflineUnspecified.into(),
                user_avatar: avatar,
                user_name: username,
            };
            let buf = rkyv_ser(&profile);
            batch.insert(make_user_profile_key(local_id).to_vec(), buf);
            svc.profile_tree
                .inner
                .apply_batch(batch)
                .map_err(ServerError::DbError)?;

            local_id
        };

        let session_token = svc.gen_auth_token();
        let session = Session {
            session_token: session_token.to_string(),
            user_id: local_user_id,
        };
        svc.valid_sessions.insert(session_token, local_user_id);

        return Ok((LoginFederatedResponse {
            session: Some(session),
        })
        .into_response());
    }

    Err(ServerError::InvalidToken.into())
}
