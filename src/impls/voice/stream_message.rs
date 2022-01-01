use std::future::Future;

use hrpc::exports::futures_util::FutureExt;
use tracing::{field, Instrument, Span};

use super::*;

pub async fn handler(
    svc: &VoiceServer,
    request: Request<()>,
    mut socket: Socket<StreamMessageResponse, StreamMessageRequest>,
) -> Result<(), HrpcServerError> {
    let user_id = svc.deps.auth(&request).await?;

    let fut = async move {
        let wait_for_initialize = socket.receive_message().map(|res| match res {
            Ok(StreamMessageRequest {
                message:
                    Some(RequestMessage::Initialize(Initialize {
                        guild_id,
                        channel_id,
                    })),
            }) => Ok((guild_id, channel_id)),
            Err(err) => Err(err.into()),
            _ => Err(zero_data_err()),
        });

        let (guild_id, channel_id) = timeout(5, wait_for_initialize).await?;
        let chan_id = (guild_id, channel_id);

        {
            let span = Span::current();
            span.record("guild", &guild_id);
            span.record("channel", &channel_id);
        }

        svc.deps
            .chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)
            .await?;

        let mut channel = svc.channels.get(&svc.worker_pool, chan_id).await?;

        socket
            .send_message(StreamMessageResponse {
                message: Some(ResponseMessage::Initialized(Initialized {
                    rtp_capabilities: into_json(channel.router().rtp_capabilities()),
                })),
            })
            .await?;

        let wait_for_prepare = socket.receive_message().map(|res| match res {
            Ok(StreamMessageRequest {
                message: Some(RequestMessage::PrepareForJoinChannel(p)),
            }) => from_json::<RtpCapabilities>(p.rtp_capabilities),
            Err(err) => Err(err.into()),
            _ => Err(zero_data_err()),
        });

        let client_capabilities = timeout(5, wait_for_prepare).await?;

        let user = User::new(&channel).await.unwrap();
        *user.inner.capabilities.lock() = Some(client_capabilities);

        let consumer_transport_options = {
            let guard = user.inner.transports.lock().await;
            let consumer = &guard.consumer;
            Some(TransportOptions {
                id: into_json(&consumer.id()),
                dtls_parameters: into_json(&consumer.dtls_parameters()),
                ice_candidates: consumer.ice_candidates().iter().map(into_json).collect(),
                ice_parameters: into_json(consumer.ice_parameters()),
            })
        };

        let producer_transport_options = {
            let guard = user.inner.transports.lock().await;
            let producer = &guard.producer;
            Some(TransportOptions {
                id: into_json(&producer.id()),
                dtls_parameters: into_json(&producer.dtls_parameters()),
                ice_candidates: producer.ice_candidates().iter().map(into_json).collect(),
                ice_parameters: into_json(producer.ice_parameters()),
            })
        };

        socket
            .send_message(StreamMessageResponse {
                message: Some(ResponseMessage::PreparedForJoinChannel(
                    PreparedForJoinChannel {
                        consumer_transport_options,
                        producer_transport_options,
                    },
                )),
            })
            .await?;

        let wait_for_join = socket.receive_message().map(|res| match res {
            Ok(StreamMessageRequest {
                message: Some(RequestMessage::JoinChannel(join)),
            }) => {
                let consumer_dtls_paramaters: DtlsParameters =
                    from_json(join.consumer_dtls_paramaters)?;
                let producer_dtls_paramaters: DtlsParameters =
                    from_json(join.producer_dtls_paramaters)?;
                let rtp_parameters: RtpParameters = from_json(join.rtp_paramaters)?;
                Ok((
                    consumer_dtls_paramaters,
                    producer_dtls_paramaters,
                    rtp_parameters,
                ))
            }
            Err(err) => Err(err.into()),
            _ => Err(zero_data_err()),
        });

        let (consumer_dtls_paramaters, producer_dtls_paramaters, rtp_parameters) =
            timeout(8, wait_for_join).await?;

        if let Err(err) = user
            .inner
            .transports
            .lock()
            .await
            .producer
            .connect(WebRtcTransportRemoteParameters {
                dtls_parameters: producer_dtls_paramaters,
            })
            .await
        {
            return Err((
                "scherzo.voice-producer-connect",
                format!("could not connect producer transport: {}", err),
            )
                .into());
        } else {
            tracing::info!("connected producer transport",);
        }
        if let Err(err) = user
            .inner
            .transports
            .lock()
            .await
            .consumer
            .connect(WebRtcTransportRemoteParameters {
                dtls_parameters: consumer_dtls_paramaters,
            })
            .await
        {
            return Err((
                "scherzo.voice-consumer-connect",
                format!("could not connect consumer transport: {}", err),
            )
                .into());
        } else {
            tracing::info!("connected consumer transport",);
        }

        let producer_options = ProducerOptions::new(MediaKind::Audio, rtp_parameters);

        match user.create_producer(producer_options).await {
            Ok(producer) => {
                tracing::info!(
                    { producer_id = %producer.id() },
                    "created producer",
                );
                *user.inner.producer.lock().await = Some(producer);
            }
            Err(err) => {
                return Err((
                    "scherzo.voice-create-producer",
                    format!("could not create producer: {}", err),
                )
                    .into());
            }
        }

        let mut other_users = Vec::new();
        for val in channel.get_all_users() {
            let maybe_rtp_capabilities = val.inner.capabilities.lock().clone();
            let maybe_producer_id = val.inner.producer.lock().await.as_ref().map(|p| p.id());
            if let (other_user_id, Some(user_producer_id), Some(user_rtp_capabilities)) =
                (*val.key(), maybe_producer_id, maybe_rtp_capabilities)
            {
                let consumer_options =
                    ConsumerOptions::new(user_producer_id, user_rtp_capabilities);

                match user.create_consumer(consumer_options).await {
                    Ok(consumer) => {
                        tracing::info!(
                            { for_producer = %user_producer_id },
                            "created consumer",
                        );
                        other_users.push(UserConsumerOptions {
                            producer_id: into_json(&user_producer_id),
                            consumer_id: into_json(&consumer.id()),
                            rtp_parameters: into_json(consumer.rtp_parameters()),
                            user_id: other_user_id,
                        });
                        user.inner.consumers.insert(consumer.id(), consumer);
                    }
                    Err(err) => {
                        tracing::error!(
                            { for_producer = %user_producer_id },
                            "could not create consumer: {}", err,
                        );
                        return Err((
                            "scherzo.voice-create-consumer",
                            format!("could not create consumer: {}", err),
                        )
                            .into());
                    }
                }
            }
        }

        if let Err((user_id, err)) = channel.add_user(user_id, user.clone()).await {
            tracing::error!({ for_participant = %user_id }, "could not create consumer: {}", err);
            return Err((
                "scherzo.voice-create-consumer",
                format!("could not create consumer: {}", err),
            )
                .into());
        } else {
            tracing::info!("user added to voice channel successfully");
        }

        socket
            .send_message(StreamMessageResponse {
                message: Some(ResponseMessage::JoinedChannel(JoinedChannel::new(
                    other_users,
                ))),
            })
            .await?;

        let (mut tx, mut rx) = socket.split();

        let process_server_events = async {
            while let Some(event) = channel.get_event().await {
                let message = match event {
                    Event::UserJoined(user_joined) => ResponseMessage::UserJoined(user_joined),
                    Event::UserLeft(user_left) => ResponseMessage::UserLeft(user_left),
                };

                bail_result!(
                    tx.send_message(StreamMessageResponse::new(Some(message)))
                        .await
                );
            }
            ServerResult::Ok(())
        };

        let process_client_messages = async {
            loop {
                let req = bail_result!(rx.receive_message().await);
                if let Some(RequestMessage::ResumeConsumer(resume)) = req.message {
                    if let Ok(consumer_id) = from_json(resume.consumer_id) {
                        let span = tracing::info_span!("process_consumer", consumer = %consumer_id);
                        let _guard = span.enter();
                        if let Err(err) = user.resume_consumer(consumer_id).await {
                            tracing::error!("could not resume consumer: {}", err);
                        } else {
                            tracing::info!("resumed consumer");
                        }
                    }
                }
            }
            #[allow(unreachable_code)]
            ServerResult::Ok(())
        };

        tokio::select! {
            res = process_server_events => {
                channel.remove_user(user_id);
                res?
            },
            res = process_client_messages => {
                channel.remove_user(user_id);
                res?
            },
        }

        Ok(())
    };

    fut.instrument(tracing::info_span!(
        "voice_messages",
        guild = field::Empty,
        channel = field::Empty,
        participant = %user_id
    ))
    .await
}

async fn timeout<T, Fut>(dur: u64, fut: Fut) -> Result<T, HrpcError>
where
    Fut: Future<Output = Result<T, HrpcError>>,
{
    let res = tokio::time::timeout(Duration::from_secs(dur), fut).await;
    match res {
        Ok(out) => out,
        Err(err) => Err(("scherzo.voice-message-timeout", err.to_string()).into()),
    }
}

fn zero_data_err() -> HrpcError {
    (
        "scherzo.voice-message-zero-data",
        "voice message from socket doesn't contain necessary data",
    )
        .into()
}
