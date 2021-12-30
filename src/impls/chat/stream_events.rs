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

    let fut = async move {
        let user_id = user_id?;
        // TODO: optimize local guild fetching
        let local_guilds: Vec<u64> = svc
            .deps
            .chat_tree
            .get_user_guilds(user_id)
            .await?
            .iter()
            .filter(|g| g.server_id == "")
            .map(|g| g.guild_id)
            .collect();

        tracing::debug!("stream events validated");

        tracing::debug!("creating stream events");
        let send_task = svc.spawn_event_stream_processor(user_id, socket);

        if let Err(err) = send_task.await {
            return Err(format!("stream events send loop task panicked: {}, aborting", err).into());
        }

        tracing::debug!("stream events ended");

        Ok(())
    };

    fut.instrument(span)
}
