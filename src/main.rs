#![recursion_limit = "256"]

use std::{convert::TryInto, time::Duration};

use dashmap::DashMap;
use harmony_rust_sdk::api::{
    auth::auth_service_server::AuthServiceServer,
    chat::{
        chat_service_server::ChatServiceServer, get_guild_invites_response::Invite,
        GetGuildResponse, GetUserResponse,
    },
    exports::{
        hrpc::{self, warp::Filter},
        prost::Message,
    },
    mediaproxy::media_proxy_service_server::MediaProxyServiceServer,
    sync::postbox_service_server::PostboxServiceServer,
};
use hrpc::warp;
use scherzo::{
    db::{
        chat::{make_invite_key, INVITE_PREFIX, USER_PREFIX},
        Db,
    },
    impls::{
        auth::AuthServer,
        chat::{ChatServer, ChatTree},
        mediaproxy::MediaproxyServer,
        sync::SyncServer,
    },
    key, ServerError,
};
use tracing::{debug, error, info, info_span, warn, Level};
use tracing_subscriber::{fmt, prelude::*};
use triomphe::Arc;

#[derive(Debug)]
pub enum Command {
    RunServer,
    GetInvites,
    GetInvite(String),
    GetMembers,
    GetMember(u64),
    GetGuilds,
    GetGuild(u64),
    GetGuildInvites(u64),
    GetGuildChannels(u64),
    GetGuildMembers(u64),
    GetChannelMessages {
        guild_id: u64,
        channel_id: u64,
        before_message_id: Option<u64>,
    },
    GetMessage {
        guild_id: u64,
        channel_id: u64,
        message_id: u64,
    },
}

// TODO: benchmark how long integrity verification takes on big `Tree`s and adjust value accordingly
const INTEGRITY_VERIFICATION_PERIOD: u64 = 60;

pub fn get_arg_as_u64(index: usize) -> Option<u64> {
    std::env::args().nth(index).and_then(|id| id.parse().ok())
}

#[tokio::main]
async fn main() {
    let mut filter_level = Level::INFO;
    let mut db_path = "db".to_string();
    let mut command = Command::RunServer;

    for (index, arg) in std::env::args().enumerate() {
        match arg.as_str() {
            "get_invites" => command = Command::GetInvites,
            "get_guilds" => command = Command::GetGuilds,
            "get_members" => command = Command::GetMembers,
            "get_guild" => {
                if let Some(id) = get_arg_as_u64(index + 1) {
                    command = Command::GetGuild(id);
                }
            }
            "get_guild_members" => {
                if let Some(id) = get_arg_as_u64(index + 1) {
                    command = Command::GetGuildMembers(id);
                }
            }
            "get_guild_channels" => {
                if let Some(id) = get_arg_as_u64(index + 1) {
                    command = Command::GetGuildChannels(id);
                }
            }
            "get_guild_invites" => {
                if let Some(id) = get_arg_as_u64(index + 1) {
                    command = Command::GetGuildInvites(id);
                }
            }
            "get_channel_messages" => {
                if let (Some(guild_id), Some(channel_id)) =
                    (get_arg_as_u64(index + 1), get_arg_as_u64(index + 2))
                {
                    command = Command::GetChannelMessages {
                        guild_id,
                        channel_id,
                        before_message_id: get_arg_as_u64(index + 3),
                    };
                }
            }
            "get_message" => {
                if let (Some(guild_id), Some(channel_id), Some(message_id)) = (
                    get_arg_as_u64(index + 1),
                    get_arg_as_u64(index + 2),
                    get_arg_as_u64(index + 3),
                ) {
                    command = Command::GetMessage {
                        guild_id,
                        channel_id,
                        message_id,
                    };
                }
            }
            "get_member" => {
                if let Some(id) = get_arg_as_u64(index + 1) {
                    command = Command::GetMember(id);
                }
            }
            "get_invite" => {
                if let Some(id) = std::env::args().nth(index + 1) {
                    command = Command::GetInvite(id);
                }
            }
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

    if !matches!(command, Command::RunServer) {
        filter_level = Level::WARN;
    }

    run_command(command, filter_level, db_path).await
}

#[cfg(feature = "sled")]
fn open_sled<P: AsRef<std::path::Path> + std::fmt::Display>(db_path: P) -> Box<dyn Db> {
    let span = info_span!("db", path = %db_path);
    let db = span.in_scope(|| {
        info!("initializing database");

        let db_result = sled::Config::new()
            .use_compression(true)
            .path(db_path)
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

fn open_db<P: AsRef<std::path::Path> + std::fmt::Display>(_db_path: P) -> Box<dyn Db> {
    #[cfg(feature = "sled")]
    return open_sled(_db_path);
    #[cfg(not(any(feature = "sled")))]
    return Box::new(scherzo::db::noop::NoopDb);
}

pub async fn run_command(command: Command, filter_level: Level, db_path: String) {
    let term_logger = fmt::layer();
    let file_appender = tracing_appender::rolling::hourly("logs", "log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    let file_logger = fmt::layer().with_ansi(false).with_writer(non_blocking);
    let filter =
        tracing_subscriber::EnvFilter::from_default_env().add_directive(filter_level.into());
    #[cfg(feature = "console")]
    let filter = filter.add_directive("tokio=trace".parse().unwrap());
    #[cfg(feature = "console")]
    let (console_layer, console_server) = console_subscriber::TasksLayer::new();

    #[cfg(not(feature = "console"))]
    let base_loggers = tracing_subscriber::registry()
        .with(filter)
        .with(term_logger)
        .with(file_logger);

    #[cfg(feature = "console")]
    let base_loggers = tracing_subscriber::registry()
        .with(filter)
        .with(console_layer)
        .with(term_logger)
        .with(file_logger);

    base_loggers.init();

    info!("logging initialized");

    #[cfg(feature = "console")]
    tokio::spawn(console_server.serve());

    let db = open_db(&db_path);

    let valid_sessions = Arc::new(DashMap::default());

    let auth_tree = db.open_tree("auth".as_bytes()).unwrap();
    let chat_tree = db.open_tree("chat".as_bytes()).unwrap();
    let sync_tree = db.open_tree("sync".as_bytes()).unwrap();

    let chat_tree = ChatTree { chat_tree };

    match command {
        Command::RunServer => {
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
                let def = Config::default();
                tokio::fs::write(config_path, toml::to_vec(&def).unwrap())
                    .await
                    .expect("failed to write default config file");
                def
            };
            // Write config file back, since it might have filled in with default values
            tokio::fs::write(config_path, toml::to_vec(&config).unwrap())
                .await
                .expect("failed to write to config file");
            debug!("running with {:?}", config);
            tokio::fs::create_dir_all(&config.media.media_root)
                .await
                .expect("could not create media root dir");
            if config.disable_ratelimits {
                warn!("rate limits are disabled, please take care!");
                scherzo::DISABLE_RATELIMITS.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            let federation_config = config.federation.map(Arc::new);

            let (dispatch_tx, dispatch_rx) = tokio::sync::mpsc::unbounded_channel();
            let keys_manager = federation_config
                .as_ref()
                .map(|conf| Arc::new(key::Manager::new(conf.key.clone())));
            let auth_server = AuthServer::new(
                chat_tree.clone(),
                auth_tree.clone(),
                valid_sessions.clone(),
                keys_manager.clone(),
                federation_config.clone(),
            );
            let chat_server =
                ChatServer::new(chat_tree.clone(), valid_sessions.clone(), dispatch_tx);
            let mediaproxy_server = MediaproxyServer::new(valid_sessions.clone());
            let sync_server = SyncServer::new(
                chat_tree.clone(),
                sync_tree,
                keys_manager,
                dispatch_rx,
                federation_config,
                config.host,
            );

            let auth = AuthServiceServer::new(auth_server).filters();
            let chat = ChatServiceServer::new(chat_server).filters();
            let rest = scherzo::impls::rest::rest(
                Arc::new(config.media.media_root),
                valid_sessions,
                config.media.max_upload_length,
            );
            let mediaproxy = MediaProxyServiceServer::new(mediaproxy_server).filters();
            let sync = PostboxServiceServer::new(sync_server).filters();

            std::thread::spawn(move || {
                let span = info_span!("db_validate");
                let _guard = span.enter();
                info!("database integrity verification task is running");
                loop {
                    std::thread::sleep(Duration::from_secs(INTEGRITY_VERIFICATION_PERIOD));
                    if let Err(err) = chat_tree
                        .chat_tree
                        .verify_integrity()
                        .and_then(|_| auth_tree.verify_integrity())
                    {
                        error!("database integrity check failed: {}", err);
                        break;
                    } else {
                        debug!("database integrity check successful");
                    }
                }
            });

            let serve = hrpc::warp::serve(
                auth.or(chat)
                    .or(mediaproxy)
                    .or(rest)
                    .or(sync)
                    .or(scherzo::impls::version())
                    .with(warp::trace::request())
                    .recover(hrpc::server::handle_rejection::<ServerError>)
                    .boxed(),
            );

            let addr = if config.listen_on_localhost {
                ([127, 0, 0, 1], config.port)
            } else {
                ([0, 0, 0, 0], config.port)
            };

            if let Some(tls_config) = config.tls {
                serve
                    .tls()
                    .cert_path(tls_config.cert_file)
                    .key_path(tls_config.key_file)
                    .run(addr)
                    .await
            } else {
                serve.run(addr).await
            }
        }
        Command::GetInvites => {
            let invites = chat_tree
                .chat_tree
                .scan_prefix(INVITE_PREFIX)
                .flatten()
                .map(|(k, v)| {
                    let invite_id = std::str::from_utf8(k.split_at(INVITE_PREFIX.len()).1).unwrap();
                    let invite_data = Invite::decode(v.as_ref()).unwrap();
                    (invite_id.to_string(), invite_data)
                });

            for (id, data) in invites {
                println!("{}: {:?}", id, data);
            }
        }
        Command::GetMembers => {
            let members = chat_tree
                .chat_tree
                .scan_prefix(USER_PREFIX)
                .flatten()
                .map(|(k, v)| {
                    let member_id =
                        u64::from_be_bytes(k.split_at(USER_PREFIX.len()).1.try_into().unwrap());
                    let member_data = GetUserResponse::decode(v.as_ref()).unwrap();
                    (member_id, member_data)
                });

            for (id, data) in members {
                println!("{}: {:?}", id, data);
            }
        }
        Command::GetGuilds => {
            let guilds = chat_tree
                .chat_tree
                .scan_prefix(&[])
                .flatten()
                .filter_map(|(k, v)| {
                    if k.len() == 8 {
                        let guild_id = u64::from_be_bytes(k.try_into().unwrap());
                        let guild_data = GetGuildResponse::decode(v.as_ref()).unwrap();

                        Some((guild_id, guild_data))
                    } else {
                        None
                    }
                });

            for (id, data) in guilds {
                println!("{}: {:?}", id, data);
            }
        }
        Command::GetGuildInvites(id) => {
            let invites = chat_tree.get_guild_invites_logic(id);
            println!("{:#?}", invites.invites)
        }
        Command::GetGuildChannels(id) => {
            let channels = chat_tree.get_guild_channels_logic(id, 0);
            println!("{:#?}", channels.channels)
        }
        Command::GetInvite(id) => {
            let invite = chat_tree
                .chat_tree
                .get(&make_invite_key(id.as_str()))
                .map(|v| v.map(|v| Invite::decode(v.as_ref()).unwrap()));
            println!("{:#?}", invite);
        }
        Command::GetMessage {
            guild_id,
            channel_id,
            message_id,
        } => {
            let message = chat_tree.get_message_logic(guild_id, channel_id, message_id);
            println!("{:#?}", message);
        }
        Command::GetChannelMessages {
            guild_id,
            channel_id,
            before_message_id,
        } => {
            let messages = chat_tree.get_channel_messages_logic(
                guild_id,
                channel_id,
                before_message_id.unwrap_or(0),
            );
            for message in messages.messages {
                println!("{:?}", message);
            }
        }
        Command::GetGuild(id) => {
            let guild = chat_tree.get_guild_logic(id);
            println!("{:#?}", guild);
        }
        Command::GetMember(id) => {
            let member = chat_tree.get_user_logic(id);
            println!("{:#?}", member);
        }
        Command::GetGuildMembers(id) => {
            let members = chat_tree.get_guild_members_logic(id);
            println!("{:?}", members.members);
        }
    }
}
