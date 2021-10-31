use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<QueryHasPermissionRequest>,
) -> ServerResult<Response<QueryHasPermissionResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    svc.chat_tree
        .query_has_permission_request(user_id, request.into_message().await?)
        .map(Response::new)
}
