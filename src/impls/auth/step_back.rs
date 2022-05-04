use super::*;

pub async fn handler(
    svc: &AuthServer,
    req: Request<StepBackRequest>,
) -> ServerResult<Response<StepBackResponse>> {
    let req = req.into_message().await?;
    let auth_id = req.auth_id;

    let prev_step;
    let Some(mut step_stack) = svc.step_map.get_mut(auth_id.as_str()) else {
        bail!(ServerError::InvalidAuthId);
    };

    // Safety: step stack can never be empty [ref:step_stack_non_empty]
    if unsafe { step_stack.last().unwrap_unchecked().can_go_back } {
        step_stack.pop();
        tracing::debug!("auth session {auth_id} went to previous step");
    } else {
        tracing::debug!("auth session {auth_id} wanted prev step, but we can't go back",);
    }

    // Safety: step stack can never be empty [ref:step_stack_non_empty]
    prev_step = unsafe { step_stack.last().unwrap_unchecked().clone() };

    drop(step_stack);

    if let Some(chan) = svc.send_step.get(auth_id.as_str()) {
        tracing::debug!("sending prev step to {auth_id} stream");
        if let Err(err) = chan.send(prev_step.clone()).await {
            tracing::error!("failed to send auth step to {auth_id}: {err}");
        }
    } else {
        tracing::debug!("no stream found for auth id {auth_id}, pushing to queue");
        svc.queued_steps
            .entry(auth_id.into())
            .and_modify(|s| s.push(prev_step.clone()))
            .or_insert_with(|| vec![prev_step.clone()]);
    }

    Ok((StepBackResponse {
        step: Some(prev_step),
    })
    .into_response())
}
