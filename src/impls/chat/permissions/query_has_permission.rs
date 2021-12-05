use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<QueryHasPermissionRequest>,
) -> ServerResult<Response<QueryHasPermissionResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;
    let request = request.into_message().await?;

    svc.deps
        .chat_tree
        .query_has_permission_request(user_id, request)
        .await
        .map(IntoResponse::into_response)
}
