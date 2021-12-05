#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(feature = "voice")]
mod voice {
    pub(super) use harmony_rust_sdk::api::voice::voice_service_server::VoiceServiceServer;
    pub(super) use scherzo::impls::voice::VoiceServer;
}

use harmony_rust_sdk::api::{
    auth::auth_service_server::AuthServiceServer,
    batch::batch_service_server::BatchServiceServer,
    chat::{
        chat_service_server::ChatServiceServer, content, guild_kind, ChannelKind, FormattedText,
        Permission,
    },
    emote::emote_service_server::EmoteServiceServer,
    exports::hrpc::{
        combine_services,
        server::transport::{http::Hyper, Transport},
    },
    mediaproxy::media_proxy_service_server::MediaProxyServiceServer,
    profile::profile_service_server::ProfileServiceServer,
    sync::postbox_service_server::PostboxServiceServer,
};
use hrpc::{
    common::layer::trace::TraceLayer as HrpcTraceLayer,
    server::{
        transport::http::{box_body, HttpConfig},
        MakeRoutes,
    },
};
use hyper::header;
use scherzo::{
    config::Config,
    db::{
        migration::{apply_migrations, get_db_version},
        Db,
    },
    impls::{
        against,
        auth::AuthServer,
        batch::BatchServer,
        chat::{AdminLogChannelLogger, ChatServer, DEFAULT_ROLE_ID},
        emote::EmoteServer,
        mediaproxy::MediaproxyServer,
        profile::ProfileServer,
        rest::RestServiceLayer,
        sync::{EventDispatch, SyncServer},
        Dependencies, HELP_TEXT,
    },
    utils,
};
use tower::limit::ConcurrencyLimitLayer;
use tower_http::{
    cors::CorsLayer,
    map_response_body::MapResponseBodyLayer,
    sensitive_headers::SetSensitiveRequestHeadersLayer,
    trace::{DefaultMakeSpan, DefaultOnFailure, DefaultOnResponse, TraceLayer},
    LatencyUnit,
};
use tracing::{debug, error, info, info_span, warn, Instrument, Level};
use tracing_subscriber::{filter::Targets, fmt, prelude::*};
use triomphe::Arc;

// in seconds
// do once per hour
// this is expensive if you have big DBs (>500mb uncompressed)
const INTEGRITY_VERIFICATION_PERIOD: u64 = 60 * 60;

fn main() {
    let mut db_path = "db".to_string();
    let mut console = false;
    let mut jaeger = false;
    let mut level_filter = Level::INFO;

    for (index, arg) in std::env::args().enumerate() {
        match arg.as_str() {
            "--db" => {
                if let Some(path) = std::env::args().nth(index + 1) {
                    db_path = path;
                }
            }
            "--enable-tokio-console" => {
                console = true;
            }
            "--enable-jaeger" => {
                jaeger = true;
            }
            "-d" | "--debug" => level_filter = Level::DEBUG,
            "-v" | "--verbose" => level_filter = Level::TRACE,
            "-q" | "--quiet" => level_filter = Level::ERROR,
            _ => {}
        }
    }

    run(db_path, console, jaeger, level_filter)
}

pub fn run(db_path: String, console: bool, jaeger: bool, log_level: Level) {
    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    let _rt_guard = rt.enter();

    let admin_logger_handle = setup_tracing(console, jaeger, log_level);

    let config = parse_config();

    let (db, current_db_version) = setup_db(&db_path, &config);

    let (deps, fed_event_receiver) = Dependencies::new(db.as_ref(), config).unwrap();

    if current_db_version == 0 {
        setup_admin_guild(deps.as_ref());
    }

    admin_logger_handle.init(&deps).unwrap();

    let (server, rest) = setup_server(deps.clone(), fed_event_receiver, log_level);

    let integrity_thread = start_integrity_check_thread(deps.as_ref());

    let transport = setup_transport(deps.as_ref(), rest);

    let serve = transport
        .serve(server)
        .instrument(info_span!("scherzo::serve"));

    rt.block_on(serve).unwrap();

    integrity_thread.join().unwrap();

    opentelemetry::global::shutdown_tracer_provider();
}

fn parse_config() -> Config {
    let config_path = std::path::Path::new("./config.toml");
    let config: Config = if config_path.exists() {
        toml::from_slice(&std::fs::read(config_path).expect("failed to read config file"))
            .expect("failed to parse config file")
    } else {
        info!("No config file found, writing default config file");
        std::fs::write(config_path, include_bytes!("../example_config.toml"))
            .expect("failed to write default config file");
        toml::from_slice(include_bytes!("../example_config.toml")).unwrap()
    };
    debug!("running with {:?}", config);
    std::fs::create_dir_all(&config.media.media_root).expect("could not create media root dir");

    if config.policy.disable_ratelimits {
        warn!("rate limits are disabled, please take care!");
    }

    config
}

fn setup_db(db_path: &str, config: &Config) -> (Box<dyn Db>, usize) {
    let db = scherzo::db::open_db(&db_path, config.db.clone());
    let (current_db_version, needs_migration) = get_db_version(db.as_ref())
        .expect("something went wrong while checking if the db needs migrations!!!");
    if needs_migration {
        // Backup db before attempting to apply migrations
        if current_db_version > 0 {
            let db_backup_name = format!("{}_backup_ver_{}", &db_path, current_db_version);
            let db_backup_path = config.db.db_backup_path.as_ref().map_or_else(
                || Path::new(&db_backup_name).to_path_buf(),
                |path| path.join(&db_backup_name),
            );
            if db_backup_path.exists() {
                warn!(
                    "there is already a backup with the same version. will not attempt to migrate"
                );
                exit(1);
            }
            warn!(
                "preparing to migrate the database, backing up to {:?}!",
                db_backup_path
            );
            copy_dir_all(Path::new(&db_path).to_path_buf(), db_backup_path)
                .expect("could not backup the db, so not applying migrations!!!");
        }

        warn!("applying database migrations!");
        apply_migrations(db.as_ref(), current_db_version)
            .expect("something went wrong while applying the migrations!!!");
    }
    (db, current_db_version)
}

fn setup_server(
    deps: Arc<Dependencies>,
    fed_event_receiver: tokio::sync::mpsc::UnboundedReceiver<EventDispatch>,
    log_level: Level,
) -> (impl MakeRoutes, RestServiceLayer) {
    let profile_server = ProfileServer::new(deps.clone());
    let emote_server = EmoteServer::new(deps.clone());
    let auth_server = AuthServer::new(deps.clone());
    let chat_server = ChatServer::new(deps.clone());
    let mediaproxy_server = MediaproxyServer::new(deps.clone());
    let sync_server = SyncServer::new(deps.clone(), fed_event_receiver);
    #[cfg(feature = "voice")]
    let voice_server = voice::VoiceServer::new(&deps, log_level);

    let profile = ProfileServiceServer::new(profile_server.clone());
    let emote = EmoteServiceServer::new(emote_server.clone());
    let auth = AuthServiceServer::new(auth_server.clone());
    let chat = ChatServiceServer::new(chat_server.clone());
    let mediaproxy = MediaProxyServiceServer::new(mediaproxy_server.clone());
    let sync = PostboxServiceServer::new(sync_server);
    #[cfg(feature = "voice")]
    let voice = voice::VoiceServiceServer::new(voice_server);

    let batchable_services = {
        let profile = ProfileServiceServer::new(profile_server.batch());
        let emote = EmoteServiceServer::new(emote_server.batch());
        let auth = AuthServiceServer::new(auth_server.batch());
        let chat = ChatServiceServer::new(chat_server.batch());
        let mediaproxy = MediaProxyServiceServer::new(mediaproxy_server.batch());
        combine_services!(profile, emote, auth, chat, mediaproxy)
    };

    let rest = RestServiceLayer::new(deps.clone());

    let batch_server = BatchServer::new(&deps, batchable_services);
    let batch = BatchServiceServer::new(batch_server);

    let server = combine_services!(
        profile,
        emote,
        auth,
        chat,
        mediaproxy,
        sync,
        #[cfg(feature = "voice")]
        voice,
        batch
    )
    .layer(HrpcTraceLayer::default_debug().span_fn(|_| tracing::debug_span!("request")));

    (server, rest)
}

fn setup_transport(
    deps: &Dependencies,
    rest: RestServiceLayer,
) -> impl Transport<Error = std::io::Error> {
    let addr: SocketAddr = if deps.config.listen_on_localhost {
        ([127, 0, 0, 1], deps.config.port).into()
    } else {
        ([0, 0, 0, 0], deps.config.port).into()
    };

    let cors = utils::either::option_layer(deps.config.cors_dev.then(CorsLayer::permissive));

    let mut transport = Hyper::new(addr)
        .expect("failed to create transport")
        .layer(cors)
        .layer(MapResponseBodyLayer::new(box_body))
        .layer(ConcurrencyLimitLayer::new(
            deps.config.policy.max_concurrent_requests,
        ))
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            header::SEC_WEBSOCKET_PROTOCOL,
        ]))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().include_headers(deps.config.log_headers))
                .on_failure(DefaultOnFailure::new().latency_unit(LatencyUnit::Micros))
                .on_response(
                    DefaultOnResponse::new()
                        .include_headers(deps.config.log_headers)
                        .latency_unit(LatencyUnit::Micros),
                ),
        )
        .layer(rest)
        .layer(against::AgainstLayer);

    if let Some(tls_config) = deps.config.tls.as_ref() {
        transport = transport
            .configure_tls_files(tls_config.cert_file.clone(), tls_config.key_file.clone());
    }

    transport.configure_hyper(
        HttpConfig::new()
            .http1_keep_alive(true)
            .http2_keep_alive_interval(Some(Duration::from_secs(10)))
            .build(),
    )
}

fn setup_tracing(console: bool, jaeger: bool, level_filter: Level) -> AdminLogChannelLogger {
    let filters = Targets::default()
        .with_targets([
            ("sled", level_filter),
            ("hyper", level_filter),
            ("tokio", Level::ERROR),
            ("runtime", Level::ERROR),
            ("console_subscriber", Level::ERROR),
            ("h2", level_filter),
            ("h2::codec", Level::ERROR),
        ])
        .with_default(level_filter);

    let (combined_logger, admin_logger_handle) = {
        let admin_handle = AdminLogChannelLogger::new();
        let admin_logger = fmt::layer().event_format(admin_handle.clone());
        let term_logger = fmt::layer();

        (
            term_logger
                .and_then(admin_logger)
                .with_filter(filters.clone()),
            admin_handle,
        )
    };

    let (console_serve_tx, console_serve_rx) = tokio::sync::oneshot::channel::<()>();
    let console_layer = if console {
        let (console_layer, console_server) = console_subscriber::TasksLayer::new();
        tokio::spawn(async move {
            if console_serve_rx.await.is_ok() {
                info_span!("scherzo::tokio_console").in_scope(|| info!("spawning console server"));
                console_server.serve().await.unwrap();
            }
        });

        Some(console_layer.with_filter(
            Targets::default().with_targets([("tokio", Level::TRACE), ("runtime", Level::TRACE)]),
        ))
    } else {
        None
    };

    let telemetry = if jaeger {
        opentelemetry::global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());
        let jaeger_tracer = opentelemetry_jaeger::new_pipeline()
            .with_service_name("scherzo")
            .install_batch(opentelemetry::runtime::Tokio)
            .expect("failed jaagere");

        Some(
            tracing_opentelemetry::layer()
                .with_tracked_inactivity(true)
                .with_tracer(jaeger_tracer)
                .with_filter(filters),
        )
    } else {
        None
    };

    tracing_subscriber::registry()
        .with(telemetry)
        .with(console_layer)
        .with(combined_logger)
        .init();
    let _ = console_serve_tx.send(());

    info!("logging initialized");

    admin_logger_handle
}

fn setup_admin_guild(deps: &Dependencies) {
    let guild_id = deps
        .chat_tree
        .create_guild_logic(
            0,
            "Admin".to_string(),
            None,
            None,
            guild_kind::Kind::new_normal(guild_kind::Normal::new()),
        )
        .unwrap();
    deps.chat_tree
        .set_permissions_logic(
            guild_id,
            None,
            DEFAULT_ROLE_ID,
            vec![Permission::new("*".to_string(), true)],
        )
        .unwrap();
    let log_id = deps
        .chat_tree
        .create_channel_logic(
            guild_id,
            "logs".to_string(),
            ChannelKind::TextUnspecified,
            None,
            None,
        )
        .unwrap();
    let cmd_id = deps
        .chat_tree
        .create_channel_logic(
            guild_id,
            "command".to_string(),
            ChannelKind::TextUnspecified,
            None,
            None,
        )
        .unwrap();
    let invite_id = format!("{}", guild_id);
    deps.chat_tree
        .create_invite_logic(guild_id, &invite_id, 1)
        .unwrap();
    deps.chat_tree
        .set_admin_guild_keys(guild_id, log_id, cmd_id)
        .unwrap();
    deps.chat_tree
        .send_with_system(
            guild_id,
            cmd_id,
            content::Content::TextMessage(content::TextContent {
                content: Some(FormattedText::new(HELP_TEXT.to_string(), Vec::new())),
            }),
        )
        .unwrap();
    warn!("admin guild created! use the invite {} to join", invite_id);
}

fn start_integrity_check_thread(deps: &Dependencies) -> std::thread::JoinHandle<()> {
    let ctt = deps.chat_tree.clone();
    let att = deps.auth_tree.clone();
    let ptt = deps.profile_tree.clone();
    let ett = deps.emote_tree.clone();
    let stt = deps.sync_tree.clone();

    std::thread::spawn(move || {
        let span = info_span!("scherzo::db");
        let _guard = span.enter();
        info!("database integrity verification task is running");
        loop {
            std::thread::sleep(Duration::from_secs(INTEGRITY_VERIFICATION_PERIOD));
            if let Err(err) = ctt
                .chat_tree
                .verify_integrity()
                .and_then(|_| att.inner.verify_integrity())
                .and_then(|_| ptt.inner.verify_integrity())
                .and_then(|_| ett.inner.verify_integrity())
                .and_then(|_| stt.verify_integrity())
            {
                error!("database integrity check failed: {}", err);
                break;
            } else {
                debug!("database integrity check successful");
            }
        }
    })
}

fn copy_dir_all(src: PathBuf, dst: PathBuf) -> std::io::Result<()> {
    use std::fs;

    fs::create_dir_all(&dst)?;
    let dir = fs::read_dir(src)?;
    for entry in dir {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn exit(code: i32) -> ! {
    opentelemetry::global::shutdown_tracer_provider();
    std::process::exit(code)
}
