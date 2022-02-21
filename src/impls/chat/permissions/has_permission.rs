use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<HasPermissionRequest>,
) -> ServerResult<Response<HasPermissionResponse>> {
    let user_id = svc.deps.auth(&request).await?;
    let request = request.into_message().await?;

    svc.deps
        .chat_tree
        .has_permission_request(user_id, request)
        .await
        .map(IntoResponse::into_response)
}
