use std::collections::HashMap;

use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetUserRolesRequest>,
) -> ServerResult<Response<GetUserRolesResponse>> {
    let user_id = svc.deps.auth(&request).await?;

    let GetUserRolesRequest {
        guild_id,
        user_ids: users_to_fetch,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    let users_to_fetch = users_to_fetch
        .is_empty()
        .then(|| vec![user_id])
        .unwrap_or(users_to_fetch);

    chat_tree.check_guild_user(guild_id, user_id).await?;
    let mut fetch_other_user = false;
    for user_to_fetch in users_to_fetch.iter().copied() {
        if user_to_fetch == user_id {
            fetch_other_user = true;
        }
        chat_tree
            .check_user_in_guild(guild_id, user_to_fetch)
            .await?;
    }
    if fetch_other_user {
        chat_tree
            .check_perms(guild_id, None, user_id, "roles.user.get", false)
            .await?;
    }

    let mut user_roles = HashMap::with_capacity(users_to_fetch.len());
    for user_to_fetch in users_to_fetch {
        let roles = chat_tree
            .get_user_roles_logic(guild_id, user_to_fetch)
            .await?;
        user_roles.insert(
            user_to_fetch,
            get_user_roles_response::UserRoles::new(roles),
        );
    }

    Ok((GetUserRolesResponse::new(user_roles)).into_response())
}
