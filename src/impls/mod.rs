pub mod admin_action;
pub mod against;
pub mod auth;
pub mod chat;
pub mod emote;
pub mod mediaproxy;
pub mod profile;
pub mod rest;
pub mod sync;
#[cfg(feature = "voice")]
pub mod voice;

use prelude::*;

use std::str::FromStr;

use crate::api::HomeserverIdentifier;
use hyper::{http, Uri};
use lettre::{
    message::{header, Mailbox, MultiPart, SinglePart},
    transport::smtp::client::{Tls, TlsParametersBuilder},
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
};
use parking_lot::Mutex;
use reqwest::Client as HttpClient;
use tokio::sync::{broadcast, mpsc};

use crate::{config::Config, key, SharedConfig, SharedConfigData};

use self::{
    auth::AuthTree, chat::ChatTree, emote::EmoteTree, profile::ProfileTree, rest::RestServiceLayer,
    sync::EventDispatch,
};

pub mod prelude {
    pub use std::{collections::HashMap, convert::TryInto, mem::size_of};

    pub use crate::{
        db::{self, deser_id, rkyv_arch, rkyv_ser, Batch, Db, DbResult, Tree},
        utils::{evec::EVec, *},
        ServerError,
    };

    pub use hrpc::{
        bail,
        response::IntoResponse,
        server::{
            error::{HrpcError as HrpcServerError, ServerResult},
            prelude::*,
            socket::Socket,
        },
        Request, Response,
    };
    pub use prost::Message as PbMessage;
    pub use rkyv::Deserialize;
    pub use scherzo_derive::*;
    pub use smol_str::SmolStr;
    pub use triomphe::Arc;

    pub use super::{auth::AuthExt, Dependencies};

    pub(crate) use super::{impl_unary_handlers, impl_ws_handlers};
}

pub type FedEventReceiver = mpsc::UnboundedReceiver<EventDispatch>;
pub type FedEventDispatcher = mpsc::UnboundedSender<EventDispatch>;

pub struct Dependencies {
    pub auth_tree: AuthTree,
    pub chat_tree: ChatTree,
    pub profile_tree: ProfileTree,
    pub emote_tree: EmoteTree,
    pub sync_tree: Tree,

    pub chat_event_sender: chat::EventSender,
    pub chat_event_canceller: chat::EventCanceller,
    pub fed_event_dispatcher: FedEventDispatcher,
    pub key_manager: Option<Arc<key::Manager>>,
    pub http: HttpClient,
    pub email: Option<AsyncSmtpTransport<Tokio1Executor>>,

    pub config: Config,
    pub runtime_config: SharedConfig,
}

impl Dependencies {
    pub async fn new(db: &Db, config: Config) -> DbResult<(Arc<Self>, FedEventReceiver)> {
        let (fed_event_dispatcher, fed_event_receiver) = mpsc::unbounded_channel();

        let auth_tree = AuthTree::new(db).await?;
        let profile_tree = ProfileTree::new(db).await?;

        let email_creds = if let Some(email) = &config.email {
            email.read_credentials().await.and_then(|res| match res {
                Ok(creds) => {
                    tracing::info!("loaded credentials for email");
                    Some(creds)
                }
                Err(err) => {
                    tracing::error!("error reading email credentials: {}", err);
                    None
                }
            })
        } else {
            tracing::debug!("email config wasn't defined, so not loading creds");
            None
        };

        let this = Self {
            auth_tree: auth_tree.clone(),
            chat_tree: ChatTree::new(db).await?,
            profile_tree: profile_tree.clone(),
            emote_tree: EmoteTree::new(db).await?,
            sync_tree: db.open_tree(b"sync").await?,

            chat_event_sender: broadcast::channel(2048).0,
            chat_event_canceller: broadcast::channel(2048).0,
            fed_event_dispatcher,
            key_manager: config
                .federation
                .as_ref()
                .map(|fc| Arc::new(key::Manager::new(fc.key.clone()))),
            http: HttpClient::new(),
            email: config.email.as_ref().map(|ec| {
                let mut builder =
                    AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&ec.server)
                        .port(ec.port);
                if ec.tls {
                    builder = builder.tls(Tls::Required(
                        // TODO: let users add root certificates
                        TlsParametersBuilder::new(ec.server.clone())
                            .build_rustls()
                            .unwrap(),
                    ));
                }
                if let Some(creds) = email_creds {
                    builder = builder.credentials(creds.into());
                }
                builder.build()
            }),

            runtime_config: Arc::new(Mutex::new(SharedConfigData {
                motd: config.motd.clone(),
            })),
            config,
        };

        Ok((Arc::new(this), fed_event_receiver))
    }
}

pub fn setup_server(
    deps: Arc<Dependencies>,
    fed_event_receiver: tokio::sync::mpsc::UnboundedReceiver<EventDispatch>,
    log_level: tracing::Level,
) -> (impl MakeRoutes, RestServiceLayer) {
    use self::{
        auth::AuthServer, chat::ChatServer, emote::EmoteServer, mediaproxy::MediaproxyServer,
        profile::ProfileServer, sync::SyncServer,
    };
    use crate::api::{
        auth::auth_service_server::AuthServiceServer, chat::chat_service_server::ChatServiceServer,
        emote::emote_service_server::EmoteServiceServer,
        mediaproxy::media_proxy_service_server::MediaProxyServiceServer,
        profile::profile_service_server::ProfileServiceServer,
        sync::postbox_service_server::PostboxServiceServer,
    };
    use hrpc::combine_services;

    let profile_server = ProfileServer::new(deps.clone());
    let emote_server = EmoteServer::new(deps.clone());
    let auth_server = AuthServer::new(deps.clone());
    let chat_server = ChatServer::new(deps.clone());
    let mediaproxy_server = MediaproxyServer::new(deps.clone());
    let sync_server = SyncServer::new(deps.clone(), fed_event_receiver);
    #[cfg(feature = "voice")]
    let voice_server = self::voice::VoiceServer::new(deps.clone(), log_level);

    let profile = ProfileServiceServer::new(profile_server.clone());
    let emote = EmoteServiceServer::new(emote_server);
    let auth = AuthServiceServer::new(auth_server);
    let chat = ChatServiceServer::new(chat_server.clone());
    let mediaproxy = MediaProxyServiceServer::new(mediaproxy_server.clone());
    let sync = PostboxServiceServer::new(sync_server);
    #[cfg(feature = "voice")]
    let voice = crate::api::voice::voice_service_server::VoiceServiceServer::new(voice_server);

    let rest = RestServiceLayer::new(deps.clone());

    let server = combine_services!(
        profile,
        emote,
        auth,
        chat,
        mediaproxy,
        sync,
        #[cfg(feature = "voice")]
        voice
    );

    (server, rest)
}

/// Panics if email is not setup in config.
pub async fn send_email(
    deps: &Dependencies,
    to: &str,
    subject: String,
    plain: String,
    html: Option<String>,
    files: Vec<(Vec<u8>, String, String, bool)>,
) -> ServerResult<()> {
    let to = to
        .parse::<Mailbox>()
        .map_err(|err| ("h.invalid-email", format!("email is invalid: {}", err)))?;
    let from = deps
        .config
        .email
        .as_ref()
        .expect("must have email config")
        .from
        .parse::<Mailbox>()
        .map_err(|err| err.to_string())?;

    let mut multipart = MultiPart::related().singlepart(SinglePart::plain(plain));
    if let Some(html) = html {
        multipart = multipart.singlepart(SinglePart::html(html));
    }
    for (file_body, file_id, file_type, is_inline) in files {
        let disposition = is_inline
            .then(header::ContentDisposition::inline)
            .unwrap_or_else(|| header::ContentDisposition::attachment(file_id.as_str()));
        multipart = multipart.singlepart(
            SinglePart::builder()
                .header(header::ContentType::parse(&file_type).unwrap())
                .header(disposition)
                .header(header::ContentId::from(file_id))
                .body(file_body),
        );
    }

    let email = lettre::Message::builder()
        .from(from)
        .to(to)
        .subject(subject)
        .date_now()
        .multipart(multipart)
        .expect("email must always be valid");

    let response = deps
        .email
        .as_ref()
        .expect("email client must be created for delete user option")
        .send(email)
        .await
        .map_err(|err| err.to_string())?;

    if !response.is_positive() {
        bail!((
            "scherzo.mailserver-error",
            format!(
                "failed to send email (code {}): {}",
                response.code(),
                response.first_line().unwrap_or("unknown error")
            )
        ));
    }

    Ok(())
}

macro_rules! impl_unary_handlers {
    ($(
        $( #[$attr:meta] )*
        $handler:ident, $req:ty, $resp:ty;
    )+) => {
        $(
            $( #[$attr] )*
            fn $handler(&self, request: Request<$req>) -> hrpc::exports::futures_util::future::BoxFuture<'_, ServerResult<Response<$resp>>> {
                Box::pin($handler::handler(self, request))
            }
        )+
    };
}

macro_rules! impl_ws_handlers {
    ($(
        $( #[$attr:meta] )*
        $handler:ident, $req:ty, $resp:ty;
    )+) => {
        $(
            $( #[$attr] )*
            fn $handler(&self, request: Request<()>, socket: Socket<$resp, $req>) -> hrpc::exports::futures_util::future::BoxFuture<'_, ServerResult<()>> {
                Box::pin($handler::handler(self, request, socket))
            }
        )+
    };
}

pub(crate) use impl_unary_handlers;
pub(crate) use impl_ws_handlers;
