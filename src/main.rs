#![feature(once_cell)]

#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use harmony_rust_sdk::api::{
    chat::{content, guild_kind, ChannelKind, FormattedText, Permission},
    exports::hrpc::server::transport::{http::Hyper, Transport},
};
use hrpc::{
    common::layer::trace::TraceLayer as HrpcTraceLayer,
    exports::futures_util::TryFutureExt,
    server::{
        transport::http::{
            box_body, layer::errid_to_status::ErrorIdentifierToStatusLayer, HttpConfig,
        },
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
        chat::{AdminGuildKeys, DEFAULT_ROLE_ID},
        rest::RestServiceLayer,
        Dependencies, HELP_TEXT,
    },
    utils, ServerError,
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

    setup_tracing(console, jaeger, log_level);
    let config = parse_config();
    let (db, current_db_version) = rt.block_on(setup_db(db_path, &config));
    let (deps, fed_event_receiver) = rt.block_on(Dependencies::new(&db, config)).unwrap();

    if current_db_version == 0 {
        rt.block_on(setup_admin_guild(deps.as_ref()));
    }

    let admin_guild_keys = rt
        .block_on(AdminGuildKeys::new(&deps.chat_tree))
        .expect("failed to get keys")
        .expect("keys must be created");
    let _ = deps.chat_tree.admin_guild_keys.set(admin_guild_keys);

    let (server, rest) = scherzo::impls::setup_server(deps.clone(), fed_event_receiver, log_level);
    let server = server
        .layer(ErrorIdentifierToStatusLayer::new(
            ServerError::identifier_to_status,
        ))
        .layer(HrpcTraceLayer::default_debug().span_fn(|req| {
            let socket_addr = req
                .extensions()
                .get::<SocketAddr>()
                .map_or_else(String::new, SocketAddr::to_string);
            tracing::debug_span!("hrpc_request", socket_addr = %socket_addr)
        }));

    let integrity = start_integrity_check_thread(deps.as_ref());

    let transport = setup_transport(deps.as_ref(), rest);
    let serve = tokio::spawn(
        transport
            .serve(server)
            .instrument(info_span!("scherzo::serve")),
    );

    rt.block_on(async {
        tokio::select! {
            biased;
            res = tokio::signal::ctrl_c() => {
                res.expect("failed to wait for signal");
            }
            res = serve => {
                res.expect("serve task panicked").expect("failed to serve");
            }
        }
    });

    tracing::info!("shutting down...");

    integrity.abort();

    if let Ok(Err(err)) = rt.block_on(tokio::time::timeout(Duration::from_secs(1), db.flush())) {
        panic!("failed to flush: {}", err);
    }

    rt.shutdown_timeout(Duration::from_secs(1));

    opentelemetry::global::shutdown_tracer_provider();

    std::process::exit(0);
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

    if config.policy.ratelimit.disable {
        warn!("rate limits are disabled, please take care!");
    }

    config
}

async fn setup_db(db_path: String, config: &Config) -> (Db, usize) {
    let db = scherzo::db::open_db(db_path.clone(), config.db.clone()).await;
    let (current_db_version, needs_migration) = get_db_version(&db)
        .await
        .expect("something went wrong while checking if the db needs migrations!!!");
    if needs_migration {
        // Backup db before attempting to apply migrations
        if current_db_version > 0 {
            let db_backup_name = format!("{}_backup_ver_{}", db_path, current_db_version);
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
        apply_migrations(&db, current_db_version)
            .await
            .expect("something went wrong while applying the migrations!!!");
    }
    (db, current_db_version)
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
    let concurrency_limiter = utils::either::option_layer(
        (deps.config.policy.max_concurrent_requests > 0)
            .then(|| ConcurrencyLimitLayer::new(deps.config.policy.max_concurrent_requests)),
    );

    let mut transport = Hyper::new(addr)
        .expect("failed to create transport")
        .layer(cors)
        .layer(MapResponseBodyLayer::new(box_body))
        .layer(concurrency_limiter)
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

fn setup_tracing(console: bool, jaeger: bool, level_filter: Level) {
    let filters = Targets::default()
        .with_targets([
            ("sled", level_filter),
            ("hyper", level_filter),
            ("tokio", Level::ERROR),
            ("runtime", Level::ERROR),
            ("console_subscriber", Level::ERROR),
            ("h2", level_filter),
            ("h2::codec", Level::ERROR),
            ("sqlx::query", level_filter),
        ])
        .with_default(level_filter);

    let term_logger = fmt::layer().with_filter(filters.clone());

    let (console_serve_tx, console_serve_rx) = tokio::sync::oneshot::channel::<()>();
    let console_layer = if console {
        let (console_layer, console_server) = console_subscriber::ConsoleLayer::new();
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
        .with(term_logger)
        .init();
    let _ = console_serve_tx.send(());

    info!("logging initialized");
}

async fn setup_admin_guild(deps: &Dependencies) {
    let guild_id = deps
        .chat_tree
        .create_guild_logic(
            0,
            "Admin".to_string(),
            None,
            None,
            guild_kind::Kind::new_normal(guild_kind::Normal::new()),
        )
        .await
        .unwrap();
    deps.chat_tree
        .set_permissions_logic(
            guild_id,
            None,
            DEFAULT_ROLE_ID,
            vec![Permission::new("*".to_string(), true)],
        )
        .await
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
        .await
        .unwrap();
    let invite_id = format!("{}", guild_id);
    deps.chat_tree
        .create_invite_logic(guild_id, &invite_id, 1)
        .await
        .unwrap();
    deps.chat_tree
        .set_admin_guild_keys(guild_id, cmd_id)
        .await
        .unwrap();
    deps.chat_tree
        .send_with_system(
            guild_id,
            cmd_id,
            content::Content::TextMessage(content::TextContent {
                content: Some(FormattedText::new(HELP_TEXT.to_string(), Vec::new())),
            }),
        )
        .await
        .unwrap();
    warn!("admin guild created! use the invite {} to join", invite_id);
}

fn start_integrity_check_thread(deps: &Dependencies) -> tokio::task::JoinHandle<()> {
    let ctt = deps.chat_tree.clone();
    let att = deps.auth_tree.clone();
    let ptt = deps.profile_tree.clone();
    let ett = deps.emote_tree.clone();
    let stt = deps.sync_tree.clone();

    let fut = async move {
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
                .await
            {
                error!("database integrity check failed: {}", err);
                break;
            } else {
                debug!("database integrity check successful");
            }
        }
    };

    tokio::spawn(fut.instrument(info_span!("scherzo::db")))
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
