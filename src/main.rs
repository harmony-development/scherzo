use std::{collections::HashMap, sync::Arc};

use harmony_rust_sdk::api::{
    auth::auth_service_server::AuthServiceServer,
    chat::chat_service_server::ChatServiceServer,
    exports::hrpc::{self, warp::Filter},
};
use hrpc::warp;
use parking_lot::Mutex;
use scherzo::{
    impls::{auth::AuthServer, chat::ChatServer},
    ServerError,
};
use tracing::{level_filters::LevelFilter, Level};
use tracing_subscriber::{fmt, prelude::*};

#[tokio::main]
async fn main() {
    let filter_level = std::env::args()
        .find(|arg| matches!(arg.as_str(), "-v" | "--verbose" | "-d" | "--debug"))
        .map_or(Level::INFO, |s| match s.as_str() {
            "-v" | "--verbose" => Level::TRACE,
            "-d" | "--debug" => Level::DEBUG,
            _ => Level::INFO,
        });

    let term_logger = fmt::layer().pretty();

    let file_appender = tracing_appender::rolling::hourly("log", "log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    let file_logger = fmt::layer().with_ansi(false).with_writer(non_blocking);

    tracing_subscriber::registry()
        .with(LevelFilter::from_level(filter_level))
        .with(term_logger)
        .with(file_logger)
        .init();

    tracing::info!("logging initialized");

    let span = tracing::info_span!("db");

    let db = span.in_scope(|| {
        tracing::info!("initializing database");

        let db_result = sled::Config::new()
            .use_compression(true)
            .path("db")
            .open()
            .map(|db| db.verify_integrity().map(|_| db));

        let db_result_flattened = match db_result {
            Ok(Ok(db)) => Ok(db),
            Ok(Err(err)) | Err(err) => Err(err),
        };

        match db_result_flattened {
            Ok(db) => db,
            Err(err) => {
                tracing::error!("cannot open database: {}; aborting", err);

                std::process::exit(1);
            }
        }
    });

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

    hrpc::warp::serve(
        auth.or(chat)
            .with(warp::trace::request())
            .recover(hrpc::server::handle_rejection::<ServerError>),
    )
    .run(([127, 0, 0, 1], 2289))
    .await
}
