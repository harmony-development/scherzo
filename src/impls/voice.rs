use std::{
    num::{NonZeroU32, NonZeroU8},
    time::Duration,
};

use super::{chat::ChatTree, prelude::*};

use ahash::RandomState;
use dashmap::DashMap;
use harmony_rust_sdk::api::{
    exports::hrpc::bail_result,
    voice::{
        stream_message_request::{Initialize, Message as RequestMessage},
        stream_message_response::{
            Initialized, JoinedChannel, Message as ResponseMessage, PreparedForJoinChannel,
            UserJoined, UserLeft,
        },
        voice_service_server::VoiceService,
        *,
    },
};
use mediasoup::{
    prelude::{
        Consumer, ConsumerId, ConsumerOptions, DtlsParameters, TransportListenIp,
        TransportListenIps, WebRtcTransport, WebRtcTransportOptions,
        WebRtcTransportRemoteParameters,
    },
    producer::{Producer, ProducerOptions},
    router::{Router, RouterOptions},
    rtp_parameters::{
        MediaKind, MimeTypeAudio, RtcpFeedback, RtpCapabilities, RtpCodecCapability,
        RtpCodecParametersParameters, RtpParameters,
    },
    transport::{ConsumeError, ProduceError, Transport},
    worker::{RequestError, Worker},
    worker_manager::WorkerManager,
};
use parking_lot::Mutex;
use tokio::{
    sync::broadcast::{self, Receiver, Sender},
    time::timeout,
};

type ChannelId = (u64, u64);
type UserId = u64;

#[derive(Clone)]
enum Event {
    UserJoined(UserJoined),
    UserLeft(UserLeft),
}

#[derive(Clone)]
pub struct VoiceServer {
    valid_sessions: SessionMap,
    worker_pool: WorkerPool,
    channels: Channels,
    chat_tree: ChatTree,
}

impl VoiceServer {
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            valid_sessions: deps.valid_sessions.clone(),
            worker_pool: WorkerPool::new(),
            channels: Channels::new(),
            chat_tree: deps.chat_tree.clone(),
        }
    }
}

impl VoiceService for VoiceServer {
    #[handler]
    async fn stream_message(
        &mut self,
        request: Request<()>,
        socket: Socket<StreamMessageRequest, StreamMessageResponse>,
    ) -> Result<(), HrpcServerError> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

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
                    tracing::error!(
                        {
                            participant = %user_id,
                        },
                        "socket error while initializing: {}",
                        err,
                    );
                    Err(())
                }
                _ => Err(()),
            }
        };

        let (guild_id, channel_id) =
            match timeout(Duration::from_secs(5), wait_for_initialize).await {
                Ok(Ok(id)) => id,
                Err(_) => {
                    tracing::error!(
                        {
                            participant = %user_id,
                        },
                        "timeouted while waiting for initialization message"
                    );
                    return Ok(());
                }
                _ => {
                    tracing::error!(
                        {
                            participant = %user_id,
                        },
                        "error occured while waiting for initialization message"
                    );
                    return Ok(());
                }
            };
        let chan_id = (guild_id, channel_id);

        if let Err(err) = self
            .chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)
        {
            tracing::error!("{}", err);
            return Ok(());
        }

        let mut channel = match self.channels.get(&self.worker_pool, chan_id).await {
            Ok(channel) => channel,
            Err(err) => {
                tracing::error!(
                    {
                        participant = %user_id,
                        guild = %guild_id,
                        channel = %channel_id,
                    },
                    "error while creating voice channel: {}",
                    err,
                );
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
                tracing::error!(
                    {
                        participant = %user_id,
                        guild = %guild_id,
                        channel = %channel_id,
                    },
                    "timeouted while waiting for prepare message"
                );
                return Ok(());
            }
            _ => {
                tracing::error!(
                    {
                        participant = %user_id,
                        guild = %guild_id,
                        channel = %channel_id,
                    },
                    "error occured while waiting for prepare message"
                );
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
                    tracing::error!(
                        {
                            participant = %user_id,
                            guild = %guild_id,
                            channel = %channel_id,
                        },
                        "socket error while waiting for join channel: {}",
                        err,
                    );
                    Err(())
                }
                _ => Err(()),
            }
        };

        let (consumer_dtls_paramaters, producer_dtls_paramaters, rtp_parameters) =
            match timeout(Duration::from_secs(8), wait_for_join).await {
                Ok(Ok(val)) => val,
                Err(_) => {
                    tracing::error!(
                        {
                            participant = %user_id,
                            guild = %guild_id,
                            channel = %channel_id,
                        },
                        "timeouted while waiting for join channel message"
                    );
                    return Ok(());
                }
                _ => {
                    tracing::error!(
                        {
                            participant = %user_id,
                            guild = %guild_id,
                            channel = %channel_id,
                        },
                        "error occured while waiting for join channel message"
                    );
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
            tracing::error!(
                {
                    participant = %user_id,
                    guild = %guild_id,
                    channel = %channel_id,
                },
                "could not connect producer transport: {}",
                err,
            );
            return Ok(());
        } else {
            tracing::info!(
                {
                    participant = %user_id,
                    guild = %guild_id,
                    channel = %channel_id,
                },
                "connected producer transport",
            );
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
            tracing::error!(
                {
                    participant = %user_id,
                    guild = %guild_id,
                    channel = %channel_id,
                },
                "could not connect consumer transport: {}",
                err,
            );
            return Ok(());
        } else {
            tracing::info!(
                {
                    participant = %user_id,
                    guild = %guild_id,
                    channel = %channel_id,
                },
                "connected consumer transport",
            );
        }

        let producer_options = ProducerOptions::new(MediaKind::Audio, rtp_parameters);

        match user.create_producer(producer_options).await {
            Ok(producer) => {
                tracing::info!(
                    {
                        participant = %user_id,
                        guild = %guild_id,
                        channel = %channel_id,
                        producer_id = %producer.id(),
                    },
                    "created producer",
                );
                *user.inner.producer.lock().await = Some(producer);
            }
            Err(err) => {
                tracing::error!(
                    {
                        participant = %user_id,
                        guild = %guild_id,
                        channel = %channel_id,
                    },
                    "could not create producer: {}",
                    err,
                );
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
                let consumer_options =
                    ConsumerOptions::new(user_producer_id, user_rtp_capabilities);

                match user.create_consumer(consumer_options).await {
                    Ok(consumer) => {
                        tracing::info!(
                            {
                                participant = %user_id,
                                guild = %guild_id,
                                channel = %channel_id,
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
                        tracing::error!(
                            {
                                participant = %user_id,
                                guild = %guild_id,
                                channel = %channel_id,
                            },
                            "could not create consumer: {}",
                            err,
                        );
                        return Ok(());
                    }
                }
            }
        }

        if let Err((user_id, err)) = channel.add_user(user_id, user.clone()).await {
            tracing::error!(
                {
                    participant = %user_id,
                    guild = %guild_id,
                    channel = %channel_id,
                },
                "could not create consumer: {}",
                err,
            );
            return Ok(());
        } else {
            tracing::info!(
                {
                    participant = %user_id,
                    guild = %guild_id,
                    channel = %channel_id,
                },
                "user added to voice channel successfully"
            );
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
                                    participant = %user_id,
                                    guild = %guild_id,
                                    channel = %channel_id,
                                    consumer_id = %consumer_id,
                                },
                                "could not resume consumer: {}",
                                err,
                            );
                        } else {
                            tracing::info!(
                                {
                                    participant = %user_id,
                                    guild = %guild_id,
                                    channel = %channel_id,
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
}

#[derive(Clone)]
struct WorkerPool {
    manager: WorkerManager,
}

impl WorkerPool {
    fn new() -> Self {
        Self {
            manager: WorkerManager::new(),
        }
    }

    async fn get(&self) -> ServerResult<Worker> {
        self.manager
            .create_worker(Default::default())
            .await
            .map_err(|err| ServerError::from(err).into())
    }
}

struct ChannelInner {
    router: Router,
    clients: DashMap<UserId, User>,
}

struct Channel {
    inner: Arc<ChannelInner>,
    event_sender: Sender<Event>,
    event_recv: Receiver<Event>,
    id: ChannelId,
}

impl Channel {
    pub async fn new(id: ChannelId, worker_pool: &WorkerPool) -> ServerResult<Self> {
        let worker = worker_pool.get().await?;
        let router = worker
            .create_router(RouterOptions::new(media_codecs()))
            .await
            .map_err(|err| ServerError::WebRTCError(err.into()))?;

        let (tx, rx) = broadcast::channel(50);

        Ok(Self {
            inner: Arc::new(ChannelInner {
                router,
                clients: DashMap::new(),
            }),
            event_sender: tx,
            event_recv: rx,
            id,
        })
    }

    /// Get router associated with this room
    pub fn router(&self) -> &Router {
        &self.inner.router
    }

    pub async fn add_user(
        &self,
        user_id: UserId,
        user: User,
    ) -> Result<(), (UserId, ConsumeError)> {
        let mut joined_datas = Vec::new();
        let maybe_producer_id = user.inner.producer.lock().await.as_ref().map(|p| p.id());
        let maybe_rtp_capabilities = user.inner.capabilities.lock().clone();
        if let (Some(user_producer_id), Some(user_rtp_capabilities)) =
            (maybe_producer_id, maybe_rtp_capabilities)
        {
            for other_user in self.inner.clients.iter_mut() {
                let other_user_id = *other_user.key();
                let consumer_options =
                    ConsumerOptions::new(user_producer_id, user_rtp_capabilities.clone());
                let consumer = other_user
                    .create_consumer(consumer_options)
                    .await
                    .map_err(|err| (other_user_id, err))?;
                tracing::info!(
                    {
                        participant = %user_id,
                        guild = %self.id.0,
                        channel = %self.id.1,
                        for_producer = %user_producer_id,
                    },
                    "created consumer",
                );
                joined_datas.push(UserConsumerOptions {
                    producer_id: into_json(&user_producer_id),
                    consumer_id: into_json(&consumer.id()),
                    rtp_parameters: into_json(consumer.rtp_parameters()),
                    user_id,
                });
                other_user.inner.consumers.insert(consumer.id(), consumer);
            }
        }
        self.inner.clients.entry(user_id).or_insert(user);
        for user_data in joined_datas {
            let _ = self.event_sender.send(Event::UserJoined(UserJoined {
                data: Some(user_data),
            }));
        }
        Ok(())
    }

    pub fn remove_user(&self, user_id: UserId) -> Option<User> {
        let user = self.inner.clients.remove(&user_id).map(|(_, user)| user);
        let _ = self
            .event_sender
            .send(Event::UserLeft(UserLeft { user_id }));
        user
    }

    pub fn get_all_users(&self) -> dashmap::iter::Iter<'_, UserId, User> {
        self.inner.clients.iter()
    }

    pub async fn get_event(&mut self) -> Option<Event> {
        self.event_recv.recv().await.ok()
    }
}

impl Clone for Channel {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            event_sender: self.event_sender.clone(),
            event_recv: self.event_sender.subscribe(),
            id: self.id,
        }
    }
}

#[derive(Clone)]
struct Channels {
    inner: Arc<DashMap<ChannelId, Channel, RandomState>>,
}

impl Channels {
    fn new() -> Self {
        Self {
            inner: DashMap::default().into(),
        }
    }

    async fn get(&self, worker_pool: &WorkerPool, channel_id: ChannelId) -> ServerResult<Channel> {
        match self.inner.entry(channel_id) {
            dashmap::mapref::entry::Entry::Occupied(entry) => Ok(entry.get().clone()),
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                let channel = Channel::new(channel_id, worker_pool).await?;
                entry.insert(channel.clone());
                Ok(channel)
            }
        }
    }
}

struct Transports {
    consumer: WebRtcTransport,
    producer: WebRtcTransport,
}

struct InnerUserData {
    capabilities: Mutex<Option<RtpCapabilities>>,
    consumers: DashMap<ConsumerId, Consumer, RandomState>,
    producer: tokio::sync::Mutex<Option<Producer>>,
    transports: tokio::sync::Mutex<Transports>,
}

#[derive(Clone)]
struct User {
    inner: Arc<InnerUserData>,
}

impl User {
    async fn new(channel: &Channel) -> ServerResult<Self> {
        let transport_options =
            WebRtcTransportOptions::new(TransportListenIps::new(TransportListenIp {
                ip: "127.0.0.1".parse().unwrap(),
                announced_ip: None,
            }));
        let producer_transport = channel
            .router()
            .create_webrtc_transport(transport_options.clone())
            .await
            .map_err(|error| ServerError::WebRTCError(error.into()))?;

        let consumer_transport = channel
            .router()
            .create_webrtc_transport(transport_options)
            .await
            .map_err(|error| ServerError::WebRTCError(error.into()))?;

        Ok(Self {
            inner: Arc::new(InnerUserData {
                capabilities: Mutex::new(None),
                consumers: DashMap::default(),
                producer: tokio::sync::Mutex::new(None),
                transports: tokio::sync::Mutex::new(Transports {
                    consumer: consumer_transport,
                    producer: producer_transport,
                }),
            }),
        })
    }

    async fn resume_consumer(&self, consumer_id: ConsumerId) -> Result<(), RequestError> {
        if let Some(consumer) = self.inner.consumers.get_mut(&consumer_id) {
            consumer.resume().await?;
        }
        Ok(())
    }

    async fn create_consumer(
        &self,
        mut options: ConsumerOptions,
    ) -> Result<Consumer, ConsumeError> {
        options.paused = true;
        let handle = tokio::runtime::Handle::current();
        let consumer_transport = self.inner.transports.lock().await.consumer.clone();
        tokio::task::spawn_blocking(move || {
            std::thread::spawn(move || {
                handle.block_on(async { consumer_transport.consume(options).await })
            })
            .join()
            .unwrap()
        })
        .await
        .unwrap()
    }

    async fn create_producer(&self, options: ProducerOptions) -> Result<Producer, ProduceError> {
        let handle = tokio::runtime::Handle::current();
        let producer_transport = self.inner.transports.lock().await.producer.clone();
        tokio::task::spawn_blocking(move || {
            std::thread::spawn(move || {
                handle.block_on(async { producer_transport.produce(options).await })
            })
            .join()
            .unwrap()
        })
        .await
        .unwrap()
    }
}

fn media_codecs() -> Vec<RtpCodecCapability> {
    vec![RtpCodecCapability::Audio {
        mime_type: MimeTypeAudio::Opus,
        preferred_payload_type: None,
        clock_rate: NonZeroU32::new(48000).unwrap(),
        channels: NonZeroU8::new(2).unwrap(),
        parameters: RtpCodecParametersParameters::from([("useinbandfec", 1_u32.into())]),
        rtcp_feedback: vec![RtcpFeedback::TransportCc],
    }]
}

fn into_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap()
}

fn from_json<T: serde::de::DeserializeOwned>(value: String) -> Result<T, ()> {
    serde_json::from_str(&value).map_err(|_| ())
}
