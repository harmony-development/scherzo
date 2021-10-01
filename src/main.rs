#![recursion_limit = "256"]

use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    time::Duration,
};

use harmony_rust_sdk::api::{
    auth::auth_service_server::AuthServiceServer,
    batch::batch_service_server::BatchServiceServer,
    chat::{chat_service_server::ChatServiceServer, ChannelKind, Permission},
    emote::emote_service_server::EmoteServiceServer,
    exports::hrpc::{self, balanced_or_tree, warp::Filter},
    mediaproxy::media_proxy_service_server::MediaProxyServiceServer,
    profile::profile_service_server::ProfileServiceServer,
    sync::postbox_service_server::PostboxServiceServer,
    voice::voice_service_server::VoiceServiceServer,
};
use hrpc::warp;
use scherzo::{
    config::DbConfig,
    db::{
        migration::{apply_migrations, get_db_version},
        Db,
    },
    impls::{
        against_proxy,
        auth::AuthServer,
        batch::BatchServer,
        chat::{AdminLogChannelLogger, ChatServer, DEFAULT_ROLE_ID},
        emote::EmoteServer,
        mediaproxy::MediaproxyServer,
        profile::ProfileServer,
        sync::SyncServer,
        voice::VoiceServer,
        Dependencies,
    },
    ServerError,
};
use tracing::{debug, error, info, info_span, warn, Level};
use tracing_subscriber::{fmt, prelude::*};

// TODO: benchmark how long integrity verification takes on big `Tree`s and adjust value accordingly
const INTEGRITY_VERIFICATION_PERIOD: u64 = 60;

#[tokio::main]
async fn main() {
    let mut filter_level = Level::INFO;
    let mut db_path = "db".to_string();

    for (index, arg) in std::env::args().enumerate() {
        match arg.as_str() {
            "-v" | "--verbose" => filter_level = Level::TRACE,
            "-d" | "--debug" => filter_level = Level::DEBUG,
            "-q" | "--quiet" => filter_level = Level::WARN,
            "-qq" => filter_level = Level::ERROR,
            "--db" => {
                if let Some(path) = std::env::args().nth(index + 1) {
                    db_path = path;
                }
            }
            _ => {}
        }
    }

    run(filter_level, db_path).await
}

#[cfg(feature = "sled")]
fn open_sled<P: AsRef<std::path::Path> + std::fmt::Display>(
    db_path: P,
    db_config: DbConfig,
) -> Box<dyn Db> {
    let span = info_span!("db", path = %db_path);
    let db = span.in_scope(|| {
        info!("initializing database");

        let db_result = sled::Config::new()
            .use_compression(true)
            .path(db_path)
            .cache_capacity(db_config.db_cache_limit)
            .mode(
                db_config
                    .sled_throughput_at_storage_cost
                    .then(|| sled::Mode::HighThroughput)
                    .unwrap_or(sled::Mode::LowSpace),
            )
            .open()
            .and_then(|db| db.verify_integrity().map(|_| db));

        match db_result {
            Ok(db) => db,
            Err(err) => {
                error!("cannot open database: {}; aborting", err);

                std::process::exit(1);
            }
        }
    });
    Box::new(db)
}

fn open_db<P: AsRef<std::path::Path> + std::fmt::Display>(
    _db_path: P,
    _db_config: DbConfig,
) -> Box<dyn Db> {
    #[cfg(feature = "sled")]
    return open_sled(_db_path, _db_config);
    #[cfg(not(any(feature = "sled")))]
    return Box::new(scherzo::db::noop::NoopDb);
}

pub async fn run(filter_level: Level, db_path: String) {
    let (wrapped_admin_logger, admin_logger_handle) = tracing_subscriber::reload::Layer::new(
        fmt::layer().event_format(AdminLogChannelLogger::empty()),
    );
    let term_logger = fmt::layer();
    let filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive(filter_level.into())
        .add_directive("rustyline=error".parse().unwrap())
        .add_directive("sled=error".parse().unwrap())
        .add_directive("hyper=error".parse().unwrap());
    #[cfg(feature = "console")]
    let filter = filter.add_directive("tokio=trace".parse().unwrap());
    #[cfg(feature = "console")]
    let (console_layer, console_server) = console_subscriber::TasksLayer::new();

    #[cfg(not(feature = "console"))]
    let base_loggers = tracing_subscriber::registry()
        .with(filter)
        .with(term_logger)
        .with(wrapped_admin_logger);

    #[cfg(feature = "console")]
    let base_loggers = tracing_subscriber::registry()
        .with(filter)
        .with(console_layer)
        .with(term_logger)
        .with(wrapped_admin_logger);

    base_loggers.init();

    info!("logging initialized");

    #[cfg(feature = "console")]
    tokio::spawn(console_server.serve());

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
            .create_guild_logic(0, "Admin".to_string(), String::new(), None)
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
        warn!("admin guild created! use the invite {} to join", invite_id);
    }

    admin_logger_handle
        .reload(fmt::layer().event_format(AdminLogChannelLogger::new(&deps)))
        .unwrap();

    let profile_server = ProfileServer::new(&deps);
    let emote_server = EmoteServer::new(&deps);
    let auth_server = AuthServer::new(&deps);
    let chat_server = ChatServer::new(&deps);
    let mediaproxy_server = MediaproxyServer::new(&deps);
    let sync_server = SyncServer::new(&deps, fed_event_receiver);
    let voice_server = VoiceServer::new(&deps);

    let profile = ProfileServiceServer::new(profile_server).filters();
    let emote = EmoteServiceServer::new(emote_server).filters();
    let auth = AuthServiceServer::new(auth_server).filters();
    let chat = ChatServiceServer::new(chat_server).filters();
    let rest = scherzo::impls::rest::rest(&deps);
    let mediaproxy = MediaProxyServiceServer::new(mediaproxy_server).filters();
    let sync = PostboxServiceServer::new(sync_server).filters();
    let voice = VoiceServiceServer::new(voice_server).filters();
    let about = scherzo::impls::about(&deps);
    let against = against_proxy();

    let filters = balanced_or_tree!(
        against, auth, chat, mediaproxy, rest, sync, voice, emote, profile, about
    )
    .boxed();

    let batch_server = BatchServer::new(
        &deps,
        filters
            .clone()
            .recover(hrpc::server::handle_rejection::<ServerError>)
            .boxed(),
    );
    let batch = BatchServiceServer::new(batch_server).filters();

    let filters = (filters.or(batch))
        .with(warp::trace::request())
        .with(warp::compression::gzip())
        .recover(hrpc::server::handle_rejection::<ServerError>)
        .boxed();

    let ctt = deps.chat_tree.clone();
    let att = deps.auth_tree.clone();
    let ptt = deps.profile_tree.clone();
    let ett = deps.emote_tree.clone();
    let stt = deps.sync_tree.clone();

    std::thread::spawn(move || {
        let span = info_span!("db_validate");
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

    let serve = hrpc::warp::serve(filters);

    let addr = if deps.config.listen_on_localhost {
        ([127, 0, 0, 1], deps.config.port)
    } else {
        ([0, 0, 0, 0], deps.config.port)
    };

    let spawn_handle = if let Some(tls_config) = deps.config.tls.as_ref() {
        tokio::spawn(
            serve
                .tls()
                .cert_path(tls_config.cert_file.clone())
                .key_path(tls_config.key_file.clone())
                .run(addr),
        )
    } else {
        tokio::spawn(serve.run(addr))
    };

    spawn_handle.await.unwrap();
}

use tokio::fs;

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
