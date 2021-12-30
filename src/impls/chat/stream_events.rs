use tracing::Instrument;

use super::*;

pub fn handler(
    svc: &ChatServer,
    request: Request<()>,
    socket: Socket<StreamEventsResponse, StreamEventsRequest>,
) -> impl Future<Output = Result<(), HrpcServerError>> + Send + '_ {
    let user_id = svc.deps.valid_sessions.auth(&request);

    let span = match &user_id {
        Ok(user_id) => tracing::debug_span!("stream_events", user_id = %user_id),
        Err(_) => tracing::debug_span!("stream_events"),
    };

    let mut cancel_recv = svc.deps.chat_event_canceller.subscribe();

    let fut = async move {
        let user_id = user_id?;
        tracing::debug!("stream events validated");

        tracing::debug!("creating stream events");
        let mut send_task = svc.spawn_event_stream_processor(user_id, socket);

        loop {
            tokio::select! {
                Ok(cancelled_user_id) = cancel_recv.recv() => {
                    if cancelled_user_id == user_id {
                        return Err(("scherzo.stream-cancelled", "stream events cancelled manually").into());
                    }
                }
                res = &mut send_task => {
                    match res {
                        Err(err) => return Err(format!("stream events send loop task panicked: {}, aborting", err).into()),
                        Ok(_) => break,
                    }
                }
                else => tokio::task::yield_now().await,
            }
        }

        tracing::debug!("stream events ended");

        Ok(())
    };

    fut.instrument(span)
}
