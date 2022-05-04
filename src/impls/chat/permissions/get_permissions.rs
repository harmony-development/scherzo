use std::collections::HashMap;

use harmony_rust_sdk::api::chat::get_permissions_response::Permissions;

use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetPermissionsRequest>,
) -> ServerResult<Response<GetPermissionsResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let GetPermissionsRequest {
        guild_id,
        channel_ids,
        role_id,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;
    let get_perms = move |channel_id| async move {
        let perms = chat_tree
            .get_permissions_logic(guild_id, channel_id, role_id)
            .await?
            .into_iter()
            .map(|(m, ok)| Permission {
                matches: m.into(),
                ok,
            })
            .collect::<Vec<_>>();
        ServerResult::Ok(Permissions::new(perms))
    };
    let check_perms = move |channel_id| async move {
        chat_tree
            .check_perms(
                guild_id,
                channel_id,
                user_id,
                "permissions.manage.get",
                false,
            )
            .await
    };

    chat_tree.check_guild_user(guild_id, user_id).await?;
    check_perms(None).await?;
    for channel_id in channel_ids.iter().copied() {
        check_perms(Some(channel_id)).await?;
    }

    let guild_perms = get_perms(None).await?;
    let mut channel_perms = HashMap::with_capacity(channel_ids.len());
    for channel_id in channel_ids {
        let chan_perms = get_perms(Some(channel_id)).await?;
        channel_perms.insert(channel_id, chan_perms);
    }

    Ok((GetPermissionsResponse {
        guild_perms: Some(guild_perms),
        channel_perms,
    })
    .into_response())
}
