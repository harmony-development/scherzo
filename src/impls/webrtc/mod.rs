use std::{
    num::{NonZeroU32, NonZeroU8},
    time::Duration,
};

use super::prelude::*;

use crate::api::{
    exports::hrpc::bail_result,
    webrtc::{web_rtc_service_server::WebRtcService, *},
};
use ahash::RandomState;
use dashmap::DashMap;
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
    worker::{RequestError, Worker, WorkerLogLevel, WorkerSettings},
    worker_manager::WorkerManager,
};
use parking_lot::Mutex;
use tokio::sync::broadcast::{self, Receiver, Sender};
use tracing::Level;

pub mod stream_message;

type ChannelId = (u64, u64);
type UserId = u64;

#[derive(Clone)]
enum Event {
    UserJoined(UserJoined),
    UserLeft(UserLeft),
}

#[derive(Clone)]
pub struct WebRtcServer {
    worker_pool: WorkerPool,
    channels: Channels,
    disable_ratelimits: bool,
    deps: Arc<Dependencies>,
}

impl WebRtcServer {
    pub fn new(deps: Arc<Dependencies>, log_level: Level) -> Self {
        Self {
            worker_pool: WorkerPool::new(log_level),
            channels: Channels::new(),
            disable_ratelimits: deps.config.policy.ratelimit.disable,
            deps,
        }
    }
}

impl WebRtcService for WebRtcServer {
    impl_ws_handlers! {
        #[rate(1, 10)]
        stream_message, StreamMessageRequest, StreamMessageResponse;
    }
}

#[derive(Clone)]
struct WorkerPool {
    manager: WorkerManager,
    log_level: WorkerLogLevel,
}

impl WorkerPool {
    fn new(log_level: Level) -> Self {
        Self {
            manager: WorkerManager::new(),
            log_level: match log_level {
                Level::ERROR => WorkerLogLevel::Error,
                Level::DEBUG => WorkerLogLevel::Debug,
                Level::WARN => WorkerLogLevel::Warn,
                _ => WorkerLogLevel::Error,
            },
        }
    }

    async fn get(&self) -> ServerResult<Worker> {
        let mut settings = WorkerSettings::default();
        settings.log_level = self.log_level;

        self.manager
            .create_worker(settings)
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

fn from_json<T: serde::de::DeserializeOwned>(value: String) -> Result<T, HrpcError> {
    serde_json::from_str(&value)
        .map_err(|err| ("scherzo.invalid-voice-json", err.to_string()).into())
}
