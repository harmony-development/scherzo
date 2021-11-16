use super::*;

pub async fn handler(
    svc: &VoiceServer,
    request: Request<()>,
    socket: Socket<StreamMessageResponse, StreamMessageRequest>,
) -> Result<(), HrpcServerError> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let span = tracing::info_span!("voice_messages", participant = %user_id);
    let _guard = span.enter();

    let wait_for_initialize = async {
        match socket.receive_message().await {
            Ok(StreamMessageRequest {
                message:
                    Some(RequestMessage::Initialize(Initialize {
                        guild_id,
                        channel_id,
                    })),
            }) => Ok((guild_id, channel_id)),
            Err(err) => {
                tracing::error!("socket error while initializing: {}", err);
                Err(())
            }
            _ => Err(()),
        }
    };

    let (guild_id, channel_id) = match timeout(Duration::from_secs(5), wait_for_initialize).await {
        Ok(Ok(id)) => id,
        Err(_) => {
            tracing::error!("timeouted while waiting for initialization message");
            return Ok(());
        }
        _ => {
            tracing::error!("error occured while waiting for initialization message",);
            return Ok(());
        }
    };
    let chan_id = (guild_id, channel_id);
    span.record("guiid", &guild_id);
    span.record("channel", &channel_id);

    if let Err(err) = svc
        .chat_tree
        .check_guild_user_channel(guild_id, user_id, channel_id)
    {
        tracing::error!("error validating IDs: {}", err);
        return Ok(());
    }

    let mut channel = match svc.channels.get(&svc.worker_pool, chan_id).await {
        Ok(channel) => channel,
        Err(err) => {
            tracing::error!("error while creating voice channel: {}", err,);
            return Ok(());
        }
    };

    socket
        .send_message(StreamMessageResponse {
            message: Some(ResponseMessage::Initialized(Initialized {
                rtp_capabilities: into_json(channel.router().rtp_capabilities()),
            })),
        })
        .await?;

    let wait_for_prepare = async {
        match socket.receive_message().await {
            Ok(StreamMessageRequest {
                message: Some(RequestMessage::PrepareForJoinChannel(p)),
            }) => from_json::<RtpCapabilities>(p.rtp_capabilities),
            _ => Err(()),
        }
    };

    let client_capabilities = match timeout(Duration::from_secs(5), wait_for_prepare).await {
        Ok(Ok(rtp_cap)) => rtp_cap,
        Err(_) => {
            tracing::error!("timeouted while waiting for prepare message");
            return Ok(());
        }
        _ => {
            tracing::error!("error occured while waiting for prepare message");
            return Ok(());
        }
    };

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

    let wait_for_join = async {
        match socket.receive_message().await {
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
            Err(err) => {
                tracing::error!("socket error while waiting for join channel: {}", err,);
                Err(())
            }
            _ => Err(()),
        }
    };

    let (consumer_dtls_paramaters, producer_dtls_paramaters, rtp_parameters) =
        match timeout(Duration::from_secs(8), wait_for_join).await {
            Ok(Ok(val)) => val,
            Err(_) => {
                tracing::error!("timeouted while waiting for join channel message");
                return Ok(());
            }
            _ => {
                tracing::error!("error occured while waiting for join channel message");
                return Ok(());
            }
        };

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
        tracing::error!("could not connect producer transport: {}", err,);
        return Ok(());
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
        tracing::error!("could not connect consumer transport: {}", err,);
        return Ok(());
    } else {
        tracing::info!("connected consumer transport",);
    }

    let producer_options = ProducerOptions::new(MediaKind::Audio, rtp_parameters);

    match user.create_producer(producer_options).await {
        Ok(producer) => {
            tracing::info!(
                {
                    producer_id = %producer.id(),
                },
                "created producer",
            );
            *user.inner.producer.lock().await = Some(producer);
        }
        Err(err) => {
            tracing::error!("could not create producer: {}", err,);
            return Ok(());
        }
    }

    let mut other_users = Vec::new();
    for val in channel.get_all_users() {
        let maybe_rtp_capabilities = val.inner.capabilities.lock().clone();
        let maybe_producer_id = val.inner.producer.lock().await.as_ref().map(|p| p.id());
        if let (other_user_id, Some(user_producer_id), Some(user_rtp_capabilities)) =
            (*val.key(), maybe_producer_id, maybe_rtp_capabilities)
        {
            let consumer_options = ConsumerOptions::new(user_producer_id, user_rtp_capabilities);

            match user.create_consumer(consumer_options).await {
                Ok(consumer) => {
                    tracing::info!(
                        {
                            for_producer = %user_producer_id,
                        },
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
                    tracing::error!("could not create consumer: {}", err,);
                    return Ok(());
                }
            }
        }
    }

    if let Err((user_id, err)) = channel.add_user(user_id, user.clone()).await {
        tracing::error!("could not create consumer: {}", err);
        return Ok(());
    } else {
        tracing::info!("user added to voice channel successfully");
    }

    socket
        .send_message(StreamMessageResponse {
            message: Some(ResponseMessage::JoinedChannel(JoinedChannel {
                other_users,
            })),
        })
        .await?;

    let process_server_events = async {
        while let Some(event) = channel.get_event().await {
            let message = match event {
                Event::UserJoined(user_joined) => ResponseMessage::UserJoined(user_joined),
                Event::UserLeft(user_left) => ResponseMessage::UserLeft(user_left),
            };

            bail_result!(
                socket
                    .send_message(StreamMessageResponse {
                        message: Some(message)
                    })
                    .await
            );
        }
        Ok(())
    };

    let process_client_messages = async {
        loop {
            let message = bail_result!(socket.receive_message().await);
            if let StreamMessageRequest {
                message: Some(RequestMessage::ResumeConsumer(resume)),
            } = message
            {
                if let Ok(consumer_id) = from_json(resume.consumer_id) {
                    if let Err(err) = user.resume_consumer(consumer_id).await {
                        tracing::error!(
                            {
                                consumer_id = %consumer_id,
                            },
                            "could not resume consumer: {}",
                            err,
                        );
                    } else {
                        tracing::info!(
                            {
                                consumer_id = %consumer_id,
                            },
                            "resumed consumer",
                        );
                    }
                }
            }
        }
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
}
