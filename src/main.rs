use std::{
    future::Future,
    net::SocketAddr,
    path::{Path, PathBuf},
    pin::Pin,
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
use hrpc::common::transport::http::box_body;
use hyper::header;
use scherzo::{
    config::DbConfig,
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
        sync::SyncServer,
        Dependencies, HELP_TEXT,
    },
};
use tower::limit::ConcurrencyLimitLayer;
use tower_http::{
    map_response_body::MapResponseBodyLayer,
    sensitive_headers::SetSensitiveRequestHeadersLayer,
    trace::{DefaultMakeSpan, DefaultOnFailure, DefaultOnRequest, DefaultOnResponse, TraceLayer},
    LatencyUnit,
};
use tracing::{debug, error, info, info_span, warn, Instrument, Level};
use tracing_subscriber::{filter::Targets, fmt, prelude::*};

// TODO: benchmark how long integrity verification takes on big `Tree`s and adjust value accordingly
const INTEGRITY_VERIFICATION_PERIOD: u64 = 60;

#[tokio::main]
async fn main() {
    let mut db_path = "db".to_string();
    let mut console = true;
    let mut level_filter = Level::INFO;

    for (index, arg) in std::env::args().enumerate() {
        match arg.as_str() {
            "--db" => {
                if let Some(path) = std::env::args().nth(index + 1) {
                    db_path = path;
                }
            }
            "--disable-console" => {
                console = false;
            }
            "-d" | "--debug" => level_filter = Level::DEBUG,
            "-v" | "--verbose" => level_filter = Level::TRACE,
            "-q" | "--quiet" => level_filter = Level::ERROR,
            _ => {}
        }
    }

    run(db_path, console, level_filter).await
}

#[cfg(feature = "sled")]
fn open_sled<P: AsRef<std::path::Path> + std::fmt::Display>(
    db_path: P,
    db_config: DbConfig,
) -> Result<Box<dyn Db>, String> {
    let result = sled::Config::new()
        .use_compression(true)
        .path(db_path)
        .cache_capacity(db_config.db_cache_limit * 1024 * 1024)
        .mode(
            db_config
                .sled_throughput_at_storage_cost
                .then(|| sled::Mode::HighThroughput)
                .unwrap_or(sled::Mode::LowSpace),
        )
        .open()
        .and_then(|db| db.verify_integrity().map(|_| db));

    match result {
        Ok(db) => Ok(Box::new(db)),
        Err(err) => Err(err.to_string()),
    }
}

fn open_db<P: AsRef<std::path::Path> + std::fmt::Display>(
    _db_path: P,
    _db_config: DbConfig,
) -> Box<dyn Db> {
    let span = info_span!("scherzo::db", path = %_db_path);
    span.in_scope(|| {
        info!("initializing database");

        #[cfg(feature = "sled")]
        let db_result = open_sled(_db_path, _db_config);
        #[cfg(not(any(feature = "sled")))]
        let db_result = Ok(Box::new(scherzo::db::noop::NoopDb));

        match db_result {
            Ok(db) => db,
            Err(err) => {
                error!("cannot open database: {}; aborting", err);

                std::process::exit(1);
            }
        }
    })
}

pub async fn run(db_path: String, console: bool, level_filter: Level) {
    let (combined_logger, admin_logger_handle) = {
        let (admin_logger, admin_logger_handle) = tracing_subscriber::reload::Layer::new(
            fmt::layer().event_format(AdminLogChannelLogger::empty()),
        );
        let term_logger = fmt::layer();

        (
            term_logger.and_then(admin_logger).with_filter(
                Targets::default()
                    .with_targets([
                        ("rustyline", Level::ERROR),
                        ("sled", Level::ERROR),
                        ("hyper", Level::ERROR),
                        ("tokio", Level::DEBUG),
                        ("runtime", Level::DEBUG),
                        ("console_subscriber::aggregator", Level::DEBUG),
                    ])
                    .with_default(level_filter),
            ),
            admin_logger_handle,
        )
    };

    let (console_serve_tx, console_serve_rx) = oneshot::channel::<()>();
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

    tracing_subscriber::registry()
        .with(console_layer)
        .with(combined_logger)
        .init();
    let _ = console_serve_tx.send(());

    info!("logging initialized");

    use scherzo::config::Config;

    let config_path = std::path::Path::new("./config.toml");
    let config: Config = if config_path.exists() {
        toml::from_slice(
            &tokio::fs::read(config_path)
                .await
                .expect("failed to read config file"),
        )
        .expect("failed to parse config file")
    } else {
        info!("No config file found, writing default config file");
        tokio::fs::write(config_path, include_bytes!("../example_config.toml"))
            .await
            .expect("failed to write default config file");
        toml::from_slice(include_bytes!("../example_config.toml")).unwrap()
    };
    debug!("running with {:?}", config);
    tokio::fs::create_dir_all(&config.media.media_root)
        .await
        .expect("could not create media root dir");
    if config.policy.disable_ratelimits {
        warn!("rate limits are disabled, please take care!");
    }

    let db = open_db(&db_path, config.db.clone());
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
            warn!(
                "preparing to migrate the database, backing up to {:?}!",
                db_backup_path
            );
            copy_dir_all(Path::new(&db_path).to_path_buf(), db_backup_path)
                .await
                .expect("could not backup the db, so not applying migrations!!!");
        }

        warn!("applying database migrations!");
        apply_migrations(db.as_ref(), current_db_version)
            .expect("something went wrong while applying the migrations!!!");
    }

    let (deps, fed_event_receiver) = Dependencies::new(db.as_ref(), config).unwrap();

    if current_db_version == 0 {
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

    admin_logger_handle
        .reload(fmt::layer().event_format(AdminLogChannelLogger::new(&deps)))
        .unwrap();

    let profile_server = ProfileServer::new(deps.clone());
    let emote_server = EmoteServer::new(deps.clone());
    let auth_server = AuthServer::new(deps.clone());
    let chat_server = ChatServer::new(deps.clone());
    let mediaproxy_server = MediaproxyServer::new(deps.clone());
    let sync_server = SyncServer::new(deps.clone(), fed_event_receiver);
    #[cfg(feature = "voice")]
    let voice_server = voice::VoiceServer::new(&deps);

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
    );

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
    });

    let addr: SocketAddr = if deps.config.listen_on_localhost {
        ([127, 0, 0, 1], deps.config.port).into()
    } else {
        ([0, 0, 0, 0], deps.config.port).into()
    };

    let mut transport = Hyper::new(addr)
        .unwrap()
        .layer(MapResponseBodyLayer::new(box_body))
        .layer(ConcurrencyLimitLayer::new(
            deps.config.policy.max_concurrent_requests,
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().include_headers(true))
                .on_failure(DefaultOnFailure::new().latency_unit(LatencyUnit::Micros))
                .on_request(DefaultOnRequest::new().level(Level::INFO))
                .on_response(
                    DefaultOnResponse::new()
                        .include_headers(true)
                        .latency_unit(LatencyUnit::Micros),
                ),
        )
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            header::SEC_WEBSOCKET_EXTENSIONS,
        ]))
        .layer(rest)
        .layer(against::AgainstLayer);

    if let Some(tls_config) = deps.config.tls.as_ref() {
        transport = transport
            .configure_tls_files(tls_config.cert_file.clone(), tls_config.key_file.clone());
    }

    let serve = transport
        .serve(server)
        .instrument(info_span!("scherzo::serve"));

    tokio::task::Builder::new()
        .name("scherzo::serve")
        .spawn(serve)
        .await
        .unwrap()
        .unwrap();
}

use tokio::{fs, sync::oneshot};

fn copy_dir_all(src: PathBuf, dst: PathBuf) -> Pin<Box<dyn Future<Output = std::io::Result<()>>>> {
    Box::pin(async move {
        fs::create_dir_all(&dst).await?;
        let mut dir = fs::read_dir(src.clone()).await?;
        while let Some(entry) = dir.next_entry().await? {
            let ty = entry.file_type().await?;
            if ty.is_dir() {
                copy_dir_all(entry.path(), dst.join(entry.file_name())).await?;
            } else {
                fs::copy(entry.path(), dst.join(entry.file_name())).await?;
            }
        }
        Ok(())
    })
}
