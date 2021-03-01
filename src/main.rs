use std::{collections::HashMap, sync::Arc};

use harmony_rust_sdk::api::{
    auth::auth_service_server::AuthServiceServer, chat::chat_service_server::ChatServiceServer,
    exports::hrpc::serve_multiple,
};
use parking_lot::Mutex;
use scherzo::{
    impls::{auth::AuthServer, chat::ChatServer},
    ServerError,
};
use simplelog::{CombinedLogger, LevelFilter, TermLogger, TerminalMode, WriteLogger};

#[tokio::main]
async fn main() {
    let filter_level = std::env::args()
        .find(|arg| matches!(arg.as_str(), "-v" | "--verbose" | "-d" | "--debug"))
        .map_or(LevelFilter::Info, |s| match s.as_str() {
            "-v" | "--verbose" => LevelFilter::Trace,
            "-d" | "--debug" => LevelFilter::Debug,
            _ => LevelFilter::Info,
        });

    CombinedLogger::init(vec![
        TermLogger::new(filter_level, Default::default(), TerminalMode::Mixed),
        WriteLogger::new(
            filter_level,
            Default::default(),
            std::fs::File::create("log").unwrap(),
        ),
    ])
    .expect("could not init logger");

    let db = sled::Config::new()
        .use_compression(true)
        .path("db")
        .open()
        .expect("couldn't open database");

    db.verify_integrity().unwrap();

    let valid_sessions = Arc::new(Mutex::new(HashMap::new()));

    let auth_tree = db.open_tree("auth").unwrap();
    let chat_tree = db.open_tree("chat").unwrap();

    let auth = AuthServiceServer::new(AuthServer::new(
        chat_tree.clone(),
        auth_tree,
        valid_sessions.clone(),
    ))
    .filters();
    let chat = ChatServiceServer::new(ChatServer::new(chat_tree, valid_sessions)).filters();

    serve_multiple!(
        addr: ([127, 0, 0, 1], 2289),
        err: ServerError,
        filters: auth, chat,
    )
    .await
}
