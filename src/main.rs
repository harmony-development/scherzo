#![recursion_limit = "256"]

use std::{
    convert::TryInto,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    time::Duration,
};

use harmony_rust_sdk::api::{
    auth::auth_service_server::AuthServiceServer,
    batch::batch_service_server::BatchServiceServer,
    chat::{chat_service_server::ChatServiceServer, Guild, Invite},
    emote::emote_service_server::EmoteServiceServer,
    exports::{
        hrpc::{self, warp::Filter},
        prost::Message,
    },
    mediaproxy::media_proxy_service_server::MediaProxyServiceServer,
    profile::{profile_service_server::ProfileServiceServer, Profile},
    sync::postbox_service_server::PostboxServiceServer,
};
use hrpc::warp;
use rustyline::{error::ReadlineError, Editor};
use scherzo::{
    db::{
        chat::{make_invite_key, INVITE_PREFIX},
        migration::{apply_migrations, get_db_version},
        profile::USER_PREFIX,
        Db,
    },
    impls::{
        against_proxy, auth::AuthServer, batch::BatchServer, chat::ChatServer, emote::EmoteServer,
        mediaproxy::MediaproxyServer, profile::ProfileServer, sync::SyncServer, Dependencies,
    },
    ServerError, SharedConfig, SharedConfigData, SCHERZO_VERSION,
};
use tracing::{debug, error, info, info_span, warn, Level};
use tracing_subscriber::{
    fmt::{
        self,
        time::{ChronoUtc, FormatTime},
    },
    prelude::*,
};

#[derive(Debug)]
pub enum Command {
    GetInvites,
    GetInvite(String),
    GetMembers,
    GetMember(u64),
    GetGuilds,
    GetGuild(u64),
    GetGuildRoles(u64),
    GetRolePerms {
        guild_id: u64,
        channel_id: u64,
        role_id: u64,
    },
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
    ShowLog(u64),
    ChangeMotd(String),
    ClearValidSessions,
    Help,
    Invalid(String),
    GetVersionInfo,
}

impl Default for Command {
    fn default() -> Self {
        Command::Invalid(String::default())
    }
}

// TODO: benchmark how long integrity verification takes on big `Tree`s and adjust value accordingly
const INTEGRITY_VERIFICATION_PERIOD: u64 = 60;

pub fn get_arg(val: &str, index: usize) -> Option<&str> {
    val.split_whitespace().nth(index)
}

pub fn get_arg_as_u64(val: &str, index: usize) -> Option<u64> {
    val.split_whitespace()
        .nth(index)
        .and_then(|a| a.parse().ok())
}

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

fn process_cmd(cmd: &str) -> Command {
    match get_arg(cmd, 0).unwrap_or("") {
        "get_invites" => Command::GetInvites,
        "get_guilds" => Command::GetGuilds,
        "get_members" => Command::GetMembers,
        "get_guild" => get_arg_as_u64(cmd, 1).map_or_else(Default::default, Command::GetGuild),
        "get_guild_members" => {
            get_arg_as_u64(cmd, 1).map_or_else(Default::default, Command::GetGuildMembers)
        }
        "get_guild_roles" => {
            get_arg_as_u64(cmd, 1).map_or_else(Default::default, Command::GetGuildRoles)
        }
        "get_role_perms" => get_arg_as_u64(cmd, 1)
            .and_then(|guild_id| get_arg_as_u64(cmd, 2).map(|role_id| (guild_id, role_id)))
            .map(|(gid, rid)| (gid, rid, get_arg_as_u64(cmd, 3).unwrap_or(0)))
            .map_or_else(Default::default, |(guild_id, role_id, channel_id)| {
                Command::GetRolePerms {
                    guild_id,
                    role_id,
                    channel_id,
                }
            }),
        "get_guild_channels" => {
            get_arg_as_u64(cmd, 1).map_or_else(Default::default, Command::GetGuildChannels)
        }
        "get_guild_invites" => {
            get_arg_as_u64(cmd, 1).map_or_else(Default::default, Command::GetGuildInvites)
        }
        "get_channel_messages" => get_arg_as_u64(cmd, 1)
            .and_then(|id| get_arg_as_u64(cmd, 2).map(|id_2| (id, id_2)))
            .map_or_else(Default::default, |(guild_id, channel_id)| {
                Command::GetChannelMessages {
                    guild_id,
                    channel_id,
                    before_message_id: get_arg_as_u64(cmd, 3),
                }
            }),
        "get_message" => get_arg_as_u64(cmd, 1)
            .and_then(|gid| {
                get_arg_as_u64(cmd, 2)
                    .and_then(|cid| get_arg_as_u64(cmd, 3).map(|mid| (gid, cid, mid)))
            })
            .map_or_else(Default::default, |(guild_id, channel_id, message_id)| {
                Command::GetMessage {
                    guild_id,
                    channel_id,
                    message_id,
                }
            }),
        "get_member" => get_arg_as_u64(cmd, 1).map_or_else(Default::default, Command::GetMember),
        "get_invite" => {
            get_arg(cmd, 1).map_or_else(Default::default, |id| Command::GetInvite(id.to_string()))
        }
        "change_motd" => get_arg(cmd, 1).map_or_else(Default::default, |motd| {
            Command::ChangeMotd(motd.to_string())
        }),
        "show_log" => get_arg_as_u64(cmd, 1).map_or_else(|| Command::ShowLog(20), Command::ShowLog),
        "help" => Command::Help,
        "clear_sessions" => Command::ClearValidSessions,
        "version" => Command::GetVersionInfo,
        x => Command::Invalid(x.to_string()),
    }
}

// TODO: write the rest of help text
const HELP_TEXT: &str = r#"
help key: command <argument: default> -> description

help -> shows help text
version -> shows version information
show_log <max_lines: 20> -> shows last log lines, limited by max_lines
change_motd <new motd> -> changes the message of the day 
clear_sessions -> clears all valid sessions from memory (not from DB)
"#;

#[cfg(feature = "sled")]
fn open_sled<P: AsRef<std::path::Path> + std::fmt::Display>(
    db_path: P,
    db_cache_limit: u64,
    sled_throughput_at_storage_cost: bool,
) -> Box<dyn Db> {
    let span = info_span!("db", path = %db_path);
    let db = span.in_scope(|| {
        info!("initializing database");

        let db_result = sled::Config::new()
            .use_compression(true)
            .path(db_path)
            .cache_capacity(db_cache_limit)
            .mode(
                sled_throughput_at_storage_cost
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
    _db_cache_limit: u64,
    _sled_throughput_at_storage_cost: bool,
) -> Box<dyn Db> {
    #[cfg(feature = "sled")]
    return open_sled(_db_path, _db_cache_limit, _sled_throughput_at_storage_cost);
    #[cfg(not(any(feature = "sled")))]
    return Box::new(scherzo::db::noop::NoopDb);
}

pub async fn run(filter_level: Level, db_path: String) {
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
        .with(file_logger);

    #[cfg(feature = "console")]
    let base_loggers = tracing_subscriber::registry()
        .with(filter)
        .with(console_layer)
        .with(file_logger);

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
        let def = Config::default();
        tokio::fs::write(config_path, toml::to_vec(&def).unwrap())
            .await
            .expect("failed to write default config file");
        def
    };
    debug!("running with {:?}", config);
    tokio::fs::create_dir_all(&config.media.media_root)
        .await
        .expect("could not create media root dir");
    if config.disable_ratelimits {
        warn!("rate limits are disabled, please take care!");
    }

    let db = open_db(
        &db_path,
        config.db_cache_limit,
        config.sled_throughput_at_storage_cost,
    );
    let (current_db_version, needs_migration) = get_db_version(db.as_ref())
        .expect("something went wrong while checking if the db needs migrations!!!");
    if needs_migration {
        // Backup db before attempting to apply migrations
        let db_backup_name = format!("{}_backup_ver_{}", &db_path, current_db_version);
        let db_backup_path = config.db_backup_path.as_ref().map_or_else(
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

        warn!("applying database migrations!");
        apply_migrations(db.as_ref(), current_db_version)
            .expect("something went wrong while applying the migrations!!!");
    }

    let (deps, fed_event_receiver) = Dependencies::new(db.as_ref(), config).unwrap();

    let profile_server = ProfileServer::new(&deps);
    let emote_server = EmoteServer::new(&deps);
    let auth_server = AuthServer::new(&deps);
    let chat_server = ChatServer::new(&deps);
    let mediaproxy_server = MediaproxyServer::new(&deps);
    let sync_server = SyncServer::new(&deps, fed_event_receiver);

    let profile = ProfileServiceServer::new(profile_server).filters();
    let emote = EmoteServiceServer::new(emote_server).filters();
    let auth = AuthServiceServer::new(auth_server).filters();
    let chat = ChatServiceServer::new(chat_server).filters();
    let rest = scherzo::impls::rest::rest(&deps);
    let mediaproxy = MediaProxyServiceServer::new(mediaproxy_server).filters();
    let sync = PostboxServiceServer::new(sync_server).filters();
    let about = scherzo::impls::about(&deps);

    let filters = against_proxy()
        .boxed()
        .or(auth)
        .boxed()
        .or(chat)
        .boxed()
        .or(mediaproxy)
        .boxed()
        .or(rest)
        .boxed()
        .or(sync)
        .boxed()
        .or(emote)
        .boxed()
        .or(profile)
        .boxed()
        .or(about)
        .boxed()
        .with(warp::trace::request())
        .boxed()
        .recover(hrpc::server::handle_rejection::<ServerError>)
        .boxed();

    let ctt = deps.chat_tree.clone();
    let att = deps.auth_tree.clone();
    std::thread::spawn(move || {
        let span = info_span!("db_validate");
        let _guard = span.enter();
        info!("database integrity verification task is running");
        loop {
            std::thread::sleep(Duration::from_secs(INTEGRITY_VERIFICATION_PERIOD));
            if let Err(err) = ctt
                .chat_tree
                .verify_integrity()
                .and_then(|_| att.verify_integrity())
            {
                error!("database integrity check failed: {}", err);
                break;
            } else {
                debug!("database integrity check successful");
            }
        }
    });

    let shared_config = SharedConfig::new(SharedConfigData::default().into());

    let batch_server = BatchServer::new(&deps, filters.clone());
    let batch = BatchServiceServer::new(batch_server).filters();
    let serve = hrpc::warp::serve(batch.or(filters));

    let addr = if deps.config.listen_on_localhost {
        ([127, 0, 0, 1], deps.config.port)
    } else {
        ([0, 0, 0, 0], deps.config.port)
    };

    if let Some(tls_config) = deps.config.tls.as_ref() {
        tokio::spawn(
            serve
                .tls()
                .cert_path(tls_config.cert_file.clone())
                .key_path(tls_config.key_file.clone())
                .run(addr),
        );
    } else {
        tokio::spawn(serve.run(addr));
    }

    let handle_cmd = |command| match command {
        Command::GetVersionInfo => {
            let db_version = get_db_version(db.as_ref()).unwrap().0;
            println!(
                "scherzo version {}, database version {}",
                SCHERZO_VERSION, db_version
            );
        }
        Command::Help => println!("{}", HELP_TEXT),
        Command::GetInvites => {
            let invites = deps
                .chat_tree
                .chat_tree
                .scan_prefix(INVITE_PREFIX)
                .map(|res| {
                    let (_, value) = res.unwrap();
                    let (id_raw, invite_raw) = value.split_at(std::mem::size_of::<u64>());
                    // Safety: this unwrap cannot fail since we split at u64 boundary
                    let id = u64::from_be_bytes(id_raw.try_into().unwrap());
                    let invite = scherzo::db::deser_invite(invite_raw.into());
                    (id, invite)
                });

            for (id, data) in invites {
                println!("{}: {:?}", id, data);
            }
        }
        Command::GetMembers => {
            let members = deps
                .profile_tree
                .inner
                .scan_prefix(USER_PREFIX)
                .flatten()
                .map(|(k, v)| {
                    let member_id =
                        u64::from_be_bytes(k.split_at(USER_PREFIX.len()).1.try_into().unwrap());
                    let member_data = Profile::decode(v.as_ref()).unwrap();
                    (member_id, member_data)
                });

            for (id, data) in members {
                println!("{}: {:?}", id, data);
            }
        }
        Command::GetGuilds => {
            let guilds = deps
                .chat_tree
                .chat_tree
                .scan_prefix(&[])
                .flatten()
                .filter_map(|(k, v)| {
                    if k.len() == 8 {
                        let guild_id = u64::from_be_bytes(k.try_into().unwrap());
                        let guild_data = Guild::decode(v.as_ref()).unwrap();

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
            let invites = deps.chat_tree.get_guild_invites_logic(id);
            println!("{:#?}", invites.invites)
        }
        Command::GetGuildChannels(id) => {
            let channels = deps.chat_tree.get_guild_channels_logic(id, 0);
            println!("{:#?}", channels.channels)
        }
        Command::GetInvite(id) => {
            let invite = deps
                .chat_tree
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
            let message = deps
                .chat_tree
                .get_message_logic(guild_id, channel_id, message_id);
            println!("{:#?}", message);
        }
        Command::GetChannelMessages {
            guild_id,
            channel_id,
            before_message_id,
        } => {
            let messages = deps.chat_tree.get_channel_messages_logic(
                guild_id,
                channel_id,
                before_message_id.unwrap_or(0),
                None,
                None,
            );
            for message in messages.messages {
                println!("{:?}", message);
            }
        }
        Command::GetGuild(id) => {
            let guild = deps.chat_tree.get_guild_logic(id);
            println!("{:#?}", guild);
        }
        Command::GetMember(id) => {
            let member = deps.profile_tree.get_profile_logic(id);
            println!("{:#?}", member);
        }
        Command::GetGuildRoles(id) => {
            let roles = deps.chat_tree.get_guild_roles_logic(id);
            println!("{:#?}", roles)
        }
        Command::GetGuildMembers(id) => {
            let members = deps.chat_tree.get_guild_members_logic(id);
            println!("{:?}", members.members);
        }
        Command::ChangeMotd(string) => {
            let mut guard = shared_config.lock();
            guard.motd.clear();
            guard.motd.push_str(&string);
        }
        Command::ShowLog(line_num) => {
            let mut log_file_path = String::from("./logs/log.");
            ChronoUtc::with_format("%Y-%m-%d-%H".to_string())
                .format_time(&mut log_file_path)
                .unwrap();
            match std::fs::read_to_string(&log_file_path) {
                Ok(log_file) => {
                    let mut lines = log_file.lines().collect::<Vec<_>>();
                    lines.drain(
                        ..lines
                            .len()
                            .saturating_sub(lines.len().min(line_num as usize)),
                    );
                    lines.reverse();

                    for line in lines {
                        println!("{}", line);
                    }
                }
                Err(err) => {
                    if err.kind() == std::io::ErrorKind::NotFound {
                        println!("log file {} not yet created", log_file_path);
                    } else {
                        println!("log file {} cant be read: {}", log_file_path, err);
                    }
                }
            }
        }
        Command::ClearValidSessions => deps.valid_sessions.clear(),
        Command::Invalid(x) => println!("invalid cmd: {}", x),
        Command::GetRolePerms {
            guild_id,
            channel_id,
            role_id,
        } => {
            let perms = deps.chat_tree.get_permissions_logic(
                guild_id,
                channel_id.eq(&0).then(|| None).unwrap_or(Some(channel_id)),
                role_id,
            );
            println!("{:#?}", perms);
        }
    };

    let mut rl = Editor::<()>::new();
    let path = std::env::var("HOME")
        .ok()
        .map(|h| Path::new(&h).join(".cache/scherzo-shell-history"))
        .unwrap_or_else(|| Path::new("/tmp/scherzo-shell-history").to_path_buf());
    if rl.load_history(&path).is_err() && rl.load_history("/tmp/scherzo-shell-history").is_err() {
        eprintln!("failed to load shell history");
    }
    loop {
        let prompt = format!(
            "({} streams) ({} valid sessions)> ",
            deps.chat_event_sender.receiver_count(),
            deps.valid_sessions.len()
        );
        let readline = rl.readline(&prompt);
        match readline {
            Ok(line) => {
                let command = process_cmd(&line);
                handle_cmd(command);
                rl.add_history_entry(line);
            }
            // We don't want to accidentally shut down the whole server
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => {}
            Err(err) => {
                println!("Error: {:?}", err);
                break;
            }
        }
    }
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
