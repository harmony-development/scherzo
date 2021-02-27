use harmony_rust_sdk::api::auth::auth_service_server::AuthServiceServer;
use scherzo::impls::auth::AuthServer;

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

    AuthServiceServer::new(AuthServer::new(db))
        .serve(([127, 0, 0, 1], 2289))
        .await
}
