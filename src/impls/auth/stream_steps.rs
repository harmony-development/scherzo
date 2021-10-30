use super::*;

pub async fn handler(
    svc: &mut AuthServer,
    _request: Request<()>,
    socket: Socket<StreamStepsRequest, StreamStepsResponse>,
) -> Result<(), HrpcServerError> {
    let msg = socket.receive_message().await?;

    let auth_id: SmolStr = msg.auth_id.into();

    if svc.step_map.contains_key(auth_id.as_str()) {
        tracing::debug!("auth id {} validated", auth_id);
    } else {
        tracing::error!("auth id {} is not valid", auth_id);
        return Err(ServerError::InvalidAuthId.into());
    }

    tracing::debug!("creating stream for id {}", auth_id);

    if let Some(mut queued_steps) = svc.queued_steps.get_mut(auth_id.as_str()) {
        for step in queued_steps.drain(..) {
            if let Err(err) = socket
                .send_message(StreamStepsResponse { step: Some(step) })
                .await
            {
                tracing::error!(
                    "error occured while sending step to id {}: {}",
                    auth_id,
                    err
                );

                // Return from func since we errored
                return Err(err);
            }
        }
    }

    let (tx, mut rx) = mpsc::channel(64);
    svc.send_step.insert(auth_id.clone(), tx);
    tracing::debug!("pushed stream tx for id {}", auth_id);

    let mut end_stream;
    while let Some(step) = rx.recv().await {
        tracing::debug!("received auth step to send to id {}", auth_id);
        end_stream = matches!(
            step,
            AuthStep {
                step: Some(auth_step::Step::Session(_)),
                ..
            }
        );

        if let Err(err) = socket
            .send_message(StreamStepsResponse { step: Some(step) })
            .await
        {
            tracing::error!(
                "error occured while sending step to id {}: {}",
                auth_id,
                err
            );

            // Break from loop since we errored
            break;
        }

        // Break if we authed
        if end_stream {
            // Close the socket
            socket.close().await;
            break;
        }
    }

    svc.send_step.remove(&auth_id);
    tracing::debug!("removing stream for id {}", auth_id);

    Ok(())
}

pub fn on_upgrade(_svc: &mut AuthServer, response: HttpResponse) -> HttpResponse {
    set_proto_name(response)
}
