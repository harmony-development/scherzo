use tokio::sync::broadcast;
use tracing::Instrument;

use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<()>,
    socket: Socket<StreamEventsResponse, StreamEventsRequest>,
) -> Result<(), HrpcServerError> {
    let user_id = svc.deps.auth(&request).await?;

    let fut = async move {
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

        let mut cancel_recv = svc.deps.chat_event_canceller.subscribe();
        let (subscribe_sender, subscribe_receiver) = broadcast::channel(2048);

        tracing::debug!("creating stream events processor");
        let mut send_task = svc.spawn_event_stream_processor(user_id, socket, subscribe_receiver);

        for guild_id in local_guilds {
            if let Err(err) = subscribe_sender.send(EventSub::Guild(guild_id)) {
                tracing::error!("failed to send guild event subscription: {:?}", err);
            }
        }
        if let Err(err) = subscribe_sender.send(EventSub::Homeserver) {
            tracing::error!("failed to send homeserver event subscription: {:?}", err);
        }
        if let Err(err) = subscribe_sender.send(EventSub::Actions) {
            tracing::error!("failed to send actions event subscription: {:?}", err);
        }

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

    fut.instrument(tracing::debug_span!("stream_events", user_id = %user_id))
        .await
}
