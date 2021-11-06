use super::*;

pub async fn handler(
    svc: &AuthServer,
    _: Request<KeyRequest>,
) -> ServerResult<Response<KeyResponse>> {
    let keys_manager = svc.keys_manager()?;
    let key = keys_manager.get_own_key().await?;

    Ok((KeyResponse {
        key: key.pk.to_vec(),
    })
    .into_response())
}
