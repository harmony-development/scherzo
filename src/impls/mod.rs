pub mod auth;
pub mod batch;
pub mod chat;
pub mod emote;
pub mod mediaproxy;
pub mod profile;
pub mod rest;
pub mod sync;
pub mod voice;

use hyper::{StatusCode, Uri};
use prelude::*;
use tower::service_fn;

use std::{str::FromStr, time::UNIX_EPOCH};

use dashmap::DashMap;
use harmony_rust_sdk::api::{
    exports::{
        hrpc::{
            body::full_box_body,
            client::{http_client, HttpClient},
            exports::http,
            HttpRequest,
        },
        prost::bytes::Bytes,
    },
    HomeserverIdentifier,
};
use parking_lot::Mutex;
use rand::Rng;
use tokio::sync::{broadcast, mpsc};

use crate::{config::Config, key, SharedConfig, SharedConfigData, SCHERZO_VERSION};

use self::{
    auth::AuthTree, chat::ChatTree, emote::EmoteTree, profile::ProfileTree, sync::EventDispatch,
};

pub mod prelude {
    pub use std::{convert::TryInto, mem::size_of};

    pub use crate::{
        db::{self, rkyv_arch, rkyv_ser, ArcTree, Batch, Db, DbResult, Tree},
        ServerError,
    };

    pub use harmony_rust_sdk::api::exports::{
        hrpc::{
            exports::{async_trait, http},
            server::{
                error::{ServerError as HrpcServerError, ServerResult},
                socket::Socket,
            },
            IntoResponse, Request, Response,
        },
        prost::Message,
    };
    pub use rkyv::Deserialize;
    pub use scherzo_derive::*;
    pub use smol_str::SmolStr;
    pub use triomphe::Arc;

    pub use super::{
        auth::{AuthExt, SessionMap},
        Dependencies,
    };
}

pub type FedEventReceiver = mpsc::UnboundedReceiver<EventDispatch>;
pub type FedEventDispatcher = mpsc::UnboundedSender<EventDispatch>;

pub struct Dependencies {
    pub auth_tree: AuthTree,
    pub chat_tree: ChatTree,
    pub profile_tree: ProfileTree,
    pub emote_tree: EmoteTree,
    pub sync_tree: ArcTree,

    pub valid_sessions: SessionMap,
    pub chat_event_sender: chat::EventSender,
    pub fed_event_dispatcher: FedEventDispatcher,
    pub key_manager: Option<Arc<key::Manager>>,
    pub action_processor: ActionProcesser,
    pub http: HttpClient,

    pub config: Config,
    pub runtime_config: SharedConfig,
}

impl Dependencies {
    pub fn new(db: &dyn Db, config: Config) -> DbResult<(Arc<Self>, FedEventReceiver)> {
        let (fed_event_dispatcher, fed_event_receiver) = mpsc::unbounded_channel();

        let auth_tree = AuthTree::new(db)?;

        let this = Self {
            auth_tree: auth_tree.clone(),
            chat_tree: ChatTree::new(db)?,
            profile_tree: ProfileTree::new(db)?,
            emote_tree: EmoteTree::new(db)?,
            sync_tree: db.open_tree(b"sync")?,

            valid_sessions: Arc::new(DashMap::default()),
            chat_event_sender: broadcast::channel(1000).0,
            fed_event_dispatcher,
            key_manager: config
                .federation
                .as_ref()
                .map(|fc| Arc::new(key::Manager::new(fc.key.clone()))),
            action_processor: ActionProcesser { auth_tree },
            http: http_client(),

            config,
            runtime_config: Arc::new(Mutex::new(SharedConfigData::default())),
        };

        Ok((Arc::new(this), fed_event_receiver))
    }
}

fn get_time_secs() -> u64 {
    UNIX_EPOCH
        .elapsed()
        .expect("time is before unix epoch")
        .as_secs()
}

fn gen_rand_inline_str() -> SmolStr {
    // Safety: arrays generated by gen_rand_arr are alphanumeric, so they are valid ASCII chars as well as UTF-8 chars [ref:alphanumeric_array_gen]
    let arr = gen_rand_arr::<_, 22>(&mut rand::thread_rng());
    let str = unsafe { std::str::from_utf8_unchecked(&arr) };
    // Safety: generated array is exactly 22 u8s long
    SmolStr::new_inline(str)
}

#[allow(dead_code)]
fn gen_rand_str<const LEN: usize>() -> SmolStr {
    let arr = gen_rand_arr::<_, LEN>(&mut rand::thread_rng());
    // Safety: arrays generated by gen_rand_arr are alphanumeric, so they are valid ASCII chars as well as UTF-8 chars [ref:alphanumeric_array_gen]
    let str = unsafe { std::str::from_utf8_unchecked(&arr) };
    SmolStr::new(str)
}

fn gen_rand_arr<RNG: Rng, const LEN: usize>(rng: &mut RNG) -> [u8; LEN] {
    let mut res = [0_u8; LEN];

    let random = rng
        .sample_iter(rand::distributions::Alphanumeric) // [tag:alphanumeric_array_gen]
        .take(LEN);

    random
        .zip(res.iter_mut())
        .for_each(|(new_ch, ch)| *ch = new_ch);

    res
}

fn gen_rand_u64() -> u64 {
    rand::thread_rng().gen_range(1..u64::MAX)
}

fn get_mimetype<T>(response: &http::Response<T>) -> &str {
    response
        .headers()
        .get(&http::header::CONTENT_TYPE)
        .and_then(|val| val.to_str().ok())
        .and_then(|s| s.split(';').next())
        .unwrap_or("application/octet-stream")
}

fn get_content_length<T>(response: &http::Response<T>) -> http::HeaderValue {
    response
        .headers()
        .get(&http::header::CONTENT_LENGTH)
        .cloned()
        .unwrap_or_else(|| unsafe {
            http::HeaderValue::from_maybe_shared_unchecked(Bytes::from_static(b"0"))
        })
}

pub mod about {
    use super::*;
    use harmony_rust_sdk::api::{
        exports::hrpc::server::{router::Routes, Service},
        rest::About,
    };

    pub struct AboutProducer {
        deps: Arc<Dependencies>,
    }

    impl Service for AboutProducer {
        fn make_routes(&self) -> Routes {
            let deps = self.deps.clone();
            let service = service_fn(move |_: HttpRequest| {
                let deps = deps.clone();
                async move {
                    let json = serde_json::to_string(&About {
                        server_name: "Scherzo".to_string(),
                        version: SCHERZO_VERSION.to_string(),
                        about_server: deps.config.server_description.clone(),
                        message_of_the_day: deps.runtime_config.lock().motd.clone(),
                    })
                    .unwrap();

                    Ok(http::Response::builder()
                        .status(StatusCode::OK)
                        .body(full_box_body(json.into_bytes().into()))
                        .unwrap())
                }
            });

            Routes::new().route("/_harmony/about", service)
        }
    }

    pub fn producer(deps: Arc<Dependencies>) -> AboutProducer {
        AboutProducer { deps }
    }
}

pub mod against {
    use harmony_rust_sdk::api::exports::hrpc::{
        body::box_body,
        server::{handler::Handler, prelude::CustomError},
    };
    use hyper::header::HeaderName;

    use super::*;

    pub struct AgainstProducer {
        http: HttpClient,
        header_name: HeaderName,
    }

    impl AgainstProducer {
        pub fn produce(&'static self) -> Handler {
            let http = self.http.clone();
            let service = service_fn(move |request: HttpRequest| {
                let http = http.clone();
                let (parts, body) = request.into_parts();
                let host_id = parts
                    .headers
                    .get(&self.header_name)
                    .and_then(|header| {
                        header
                            .to_str()
                            .ok()
                            .and_then(|v| HomeserverIdentifier::from_str(v).ok())
                    })
                    .unwrap();
                let url = {
                    let mut url_parts = host_id.to_url().into_parts();
                    url_parts.path_and_query = Some(parts.uri.path().parse().unwrap());
                    Uri::from_parts(url_parts).unwrap()
                };
                async move {
                    let mut request = http::Request::builder()
                        .uri(url)
                        .method(parts.method)
                        .body(body)
                        .unwrap();
                    *request.headers_mut() = parts.headers;
                    *request.extensions_mut() = parts.extensions;

                    let host_response = http
                        .request(request)
                        .await
                        .map(|r| r.map(box_body))
                        .unwrap_or_else(|err| ServerError::HttpError(err).as_error_response());

                    Ok(host_response)
                }
            });
            Handler::new(service)
        }
    }

    pub fn producer() -> AgainstProducer {
        AgainstProducer {
            http: http_client(),
            header_name: HeaderName::from_static("against"),
        }
    }
}

pub struct AdminActionError;

#[derive(Debug, Clone, Copy)]
pub enum AdminAction {
    GenerateRegistrationToken,
}

impl FromStr for AdminAction {
    type Err = AdminActionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let act = match s.trim_start_matches('/').trim() {
            "generate registration-token" => AdminAction::GenerateRegistrationToken,
            _ => return Err(AdminActionError),
        };
        Ok(act)
    }
}

#[derive(Clone)]
pub struct ActionProcesser {
    auth_tree: AuthTree,
}

impl ActionProcesser {
    pub fn run(&self, action: &str) -> ServerResult<String> {
        let maybe_action = AdminAction::from_str(action);
        match maybe_action {
            Ok(action) => match action {
                AdminAction::GenerateRegistrationToken => {
                    let token = self.auth_tree.put_rand_reg_token()?;
                    Ok(token.into())
                }
            },
            Err(_) => Ok(format!("invalid command: `{}`", action)),
        }
    }
}
