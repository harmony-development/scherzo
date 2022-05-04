use super::*;

pub async fn handler(
    svc: &AuthServer,
    _: Request<BeginAuthRequest>,
) -> ServerResult<Response<BeginAuthResponse>> {
    let auth_id = gen_rand_inline_str();

    // [tag:step_stack_non_empty]
    svc.step_map
        .entry(auth_id.clone())
        .and_modify(|s| *s = vec![initial_auth_step()])
        .or_insert_with(|| vec![initial_auth_step()]);

    tracing::debug!("new auth session {auth_id}");

    Ok((BeginAuthResponse {
        auth_id: auth_id.into(),
    })
    .into_response())
}
