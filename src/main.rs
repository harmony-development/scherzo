use std::{collections::HashMap, convert::TryInto, sync::Arc, time::Instant};

use harmony_rust_sdk::api::{
    auth::auth_service_server::AuthServiceServer,
    chat::{
        chat_service_server::{ChatService, ChatServiceServer},
        get_guild_invites_response::Invite,
        GetChannelMessagesRequest, GetGuildChannelsRequest, GetGuildInvitesRequest,
        GetGuildMembersRequest, GetGuildRequest, GetGuildResponse, GetMessageRequest,
        GetUserRequest, GetUserResponse,
    },
    exports::{
        hrpc::{self, warp::Filter, IntoRequest},
        prost::Message,
    },
};
use hrpc::warp;
use parking_lot::Mutex;
use scherzo::{
    db::{make_invite_key, INVITE_PREFIX, USER_PREFIX},
    impls::{auth::AuthServer, chat::ChatServer},
    ServerError,
};
use tracing::{level_filters::LevelFilter, Level};
use tracing_subscriber::{fmt, prelude::*};

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
    std::env::args()
        .nth(index)
        .map(|id| id.parse().ok())
        .flatten()
}

#[tokio::main]
async fn main() {
    let mut filter_level = Level::INFO;
    let mut db_path = "db".to_string();
    let mut command = Command::RunServer;

    for (index, arg) in std::env::args().enumerate() {
        if index == 1 && !arg.starts_with('-') {
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
                _ => {}
            }
        }

        match arg.as_str() {
            "-v" | "--verbose" => filter_level = Level::TRACE,
            "-d" | "--debug" => filter_level = Level::DEBUG,
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

pub async fn run_command(command: Command, filter_level: Level, db_path: String) {
    let term_logger = fmt::layer().pretty();

    let file_appender = tracing_appender::rolling::hourly("logs", "log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    let file_logger = fmt::layer().with_ansi(false).with_writer(non_blocking);

    tracing_subscriber::registry()
        .with(LevelFilter::from_level(filter_level))
        .with(term_logger)
        .with(file_logger)
        .init();

    tracing::info!("logging initialized");

    let span = tracing::info_span!("db", path = %db_path);
    let db = span.in_scope(|| {
        tracing::info!("initializing database");

        let db_result = sled::Config::new()
            .use_compression(true)
            .path(db_path)
            .open()
            .and_then(|db| db.verify_integrity().map(|_| db));

        match db_result {
            Ok(db) => db,
            Err(err) => {
                tracing::error!("cannot open database: {}; aborting", err);

                std::process::exit(1);
            }
        }
    });
    drop(span);

    let valid_sessions = Arc::new(Mutex::new(HashMap::new()));

    let auth_tree = db.open_tree("auth").unwrap();
    let chat_tree = db.open_tree("chat").unwrap();

    let auth_server = AuthServer::new(chat_tree.clone(), auth_tree.clone(), valid_sessions.clone());
    let mut chat_server = ChatServer::new(chat_tree.clone(), valid_sessions);

    match command {
        Command::RunServer => {
            chat_server.check_auth = true;
            let auth = AuthServiceServer::new(auth_server).filters();
            let chat = ChatServiceServer::new(chat_server).filters();

            std::thread::spawn(move || {
                let mut last = Instant::now();
                let span = tracing::info_span!("db_validate");
                let _guard = span.enter();
                tracing::info!("database integrity verification task is running");
                loop {
                    if last.elapsed().as_secs() > INTEGRITY_VERIFICATION_PERIOD {
                        if let Err(err) = chat_tree
                            .verify_integrity()
                            .and_then(|_| auth_tree.verify_integrity())
                        {
                            tracing::error!("database integrity check failed: {}", err);
                            break;
                        } else {
                            tracing::info!("database integrity check successful");
                        }
                        last = Instant::now();
                    }
                }
            });

            hrpc::warp::serve(
                auth.or(chat)
                    .with(warp::trace::request())
                    .recover(hrpc::server::handle_rejection::<ServerError>),
            )
            .run(([127, 0, 0, 1], 2289))
            .await
        }
        Command::GetInvites => {
            let invites = chat_tree
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
            let members = chat_tree.scan_prefix(USER_PREFIX).flatten().map(|(k, v)| {
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
            let guilds = chat_tree.scan_prefix([]).flatten().filter_map(|(k, v)| {
                if k.len() == 8 {
                    let guild_id = u64::from_be_bytes(k.as_ref().try_into().unwrap());
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
            chat_server.check_auth = false;
            let invites = chat_server
                .get_guild_invites(GetGuildInvitesRequest { guild_id: id }.into_request())
                .await
                .map(|r| r.invites);
            println!("{:#?}", invites)
        }
        Command::GetGuildChannels(id) => {
            chat_server.check_auth = false;
            let channels = chat_server
                .get_guild_channels(GetGuildChannelsRequest { guild_id: id }.into_request())
                .await
                .map(|r| r.channels);
            println!("{:#?}", channels)
        }
        Command::GetInvite(id) => {
            let invite = chat_tree
                .get(make_invite_key(id.as_str()))
                .map(|v| v.map(|v| Invite::decode(v.as_ref()).unwrap()));
            println!("{:#?}", invite);
        }
        Command::GetMessage {
            guild_id,
            channel_id,
            message_id,
        } => {
            chat_server.check_auth = false;
            let message = chat_server
                .get_message(
                    GetMessageRequest {
                        guild_id,
                        channel_id,
                        message_id,
                    }
                    .into_request(),
                )
                .await;
            println!("{:#?}", message);
        }
        Command::GetChannelMessages {
            guild_id,
            channel_id,
            before_message_id,
        } => {
            chat_server.check_auth = false;
            let messages = chat_server
                .get_channel_messages(
                    GetChannelMessagesRequest {
                        guild_id,
                        channel_id,
                        before_message: before_message_id.unwrap_or(0),
                    }
                    .into_request(),
                )
                .await
                .map(|r| r.messages);
            match messages {
                Ok(messages) => {
                    for message in messages {
                        println!("{:?}", message);
                    }
                }
                Err(err) => {
                    eprintln!("{}", err);
                }
            }
        }
        Command::GetGuild(id) => {
            chat_server.check_auth = false;
            let guild = chat_server
                .get_guild(GetGuildRequest { guild_id: id }.into_request())
                .await;
            println!("{:#?}", guild);
        }
        Command::GetMember(id) => {
            chat_server.check_auth = false;
            let member = chat_server
                .get_user(GetUserRequest { user_id: id }.into_request())
                .await;
            println!("{:#?}", member);
        }
        Command::GetGuildMembers(id) => {
            chat_server.check_auth = false;
            let members = chat_server
                .get_guild_members(GetGuildMembersRequest { guild_id: id }.into_request())
                .await
                .map(|r| r.members);
            println!("{:?}", members);
        }
    }
}
