use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<()>,
    socket: Socket<StreamEventsRequest, StreamEventsResponse>,
) -> Result<(), HrpcServerError> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;
    tracing::debug!("stream events validated for user {}", user_id);

    tracing::debug!("creating stream events for user {}", user_id);
    let (sub_tx, sub_rx) = mpsc::channel(64);
    let chat_tree = svc.chat_tree.clone();

    let send_loop = svc.spawn_event_stream_processor(user_id, sub_rx, socket.clone());
    let recv_loop = async move {
        loop {
            let req = bail_result!(socket.receive_message().await);
            if let Some(req) = req.request {
                use stream_events_request::*;

                tracing::debug!("got new stream events request for user {}", user_id);

                let sub = match req {
                    Request::SubscribeToGuild(SubscribeToGuild { guild_id }) => {
                        match chat_tree.check_guild_user(guild_id, user_id) {
                            Ok(_) => EventSub::Guild(guild_id),
                            Err(err) => {
                                tracing::error!("{}", err);
                                continue;
                            }
                        }
                    }
                    Request::SubscribeToActions(SubscribeToActions {}) => EventSub::Actions,
                    Request::SubscribeToHomeserverEvents(SubscribeToHomeserverEvents {}) => {
                        EventSub::Homeserver
                    }
                };

                drop(sub_tx.send(sub).await);
            }
        }
        #[allow(unreachable_code)]
        ServerResult::Ok(())
    };

    tokio::select!(
        res = send_loop => {
            res.unwrap();
        }
        res = tokio::spawn(recv_loop) => {
            res.unwrap()?;
        }
    );
    tracing::debug!("stream events ended for user {}", user_id);

    Ok(())
}

pub fn on_upgrade(
    _svc: &mut ChatServer,
    response: http::Response<BoxBody>,
) -> http::Response<BoxBody> {
    set_proto_name(response)
}