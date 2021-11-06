use super::*;

pub async fn handler(
    svc: &AuthServer,
    _: Request<BeginAuthRequest>,
) -> ServerResult<Response<BeginAuthResponse>> {
    let initial_step = AuthStep {
        can_go_back: false,
        fallback_url: String::default(),
        step: Some(auth_step::Step::Choice(auth_step::Choice {
            title: "initial".to_string(),
            options: ["login", "register"]
                .iter()
                .map(ToString::to_string)
                .collect(),
        })),
    };

    let auth_id = gen_rand_inline_str();

    // [tag:step_stack_non_empty]
    svc.step_map
        .entry(auth_id.clone())
        .and_modify(|s| *s = vec![initial_step.clone()])
        .or_insert_with(|| vec![initial_step.clone()]);

    tracing::debug!("new auth session {}", auth_id);

    Ok((BeginAuthResponse {
        auth_id: auth_id.into(),
    })
    .into_response())
}
