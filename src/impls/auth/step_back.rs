use super::*;

pub async fn handler(
    svc: &mut AuthServer,
    req: Request<StepBackRequest>,
) -> ServerResult<Response<StepBackResponse>> {
    let req = req.into_message().await?;
    let auth_id = req.auth_id;

    let prev_step;

    if let Some(mut step_stack) = svc.step_map.get_mut(auth_id.as_str()) {
        // Safety: step stack can never be empty [ref:step_stack_non_empty]
        if unsafe { step_stack.last().unwrap_unchecked().can_go_back } {
            step_stack.pop();
            tracing::debug!("auth session {} went to previous step", auth_id);
        } else {
            tracing::debug!(
                "auth session {} wanted prev step, but we can't go back",
                auth_id
            );
        }
        // Safety: step stack can never be empty [ref:step_stack_non_empty]
        prev_step = unsafe { step_stack.last().unwrap_unchecked().clone() };
        if let Some(chan) = svc.send_step.get(auth_id.as_str()) {
            tracing::debug!("sending prev step to {} stream", auth_id);
            if let Err(err) = chan.send(prev_step.clone()).await {
                tracing::error!("failed to send auth step to {}: {}", auth_id, err);
            }
        } else {
            tracing::debug!("no stream found for auth id {}, pushing to queue", auth_id);
            svc.queued_steps
                .entry(auth_id.into())
                .and_modify(|s| s.push(prev_step.clone()))
                .or_insert_with(|| vec![prev_step.clone()]);
        }
    } else {
        return Err(ServerError::InvalidAuthId.into());
    }

    Ok((StepBackResponse {
        step: Some(prev_step),
    })
    .into_response())
}
