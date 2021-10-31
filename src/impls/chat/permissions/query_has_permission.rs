use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<QueryHasPermissionRequest>,
) -> ServerResult<Response<QueryHasPermissionResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    svc.deps
        .chat_tree
        .query_has_permission_request(user_id, request.into_message().await?)
        .map(Response::new)
}
