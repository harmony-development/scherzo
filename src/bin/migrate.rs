use hrpc::BoxError;
use scherzo::db::{sled::shared as sled, sqlite::shared as sqlite};

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let db_path = std::env::args().nth(1).expect("expected db path");
    let db_type = std::env::args()
        .nth(2)
        .expect("expected db type to migrate to");
    let db_config = scherzo::config::DbConfig::default();

    let sqlite_db = sqlite::open_database(db_path.clone(), db_config.clone()).await;
    let sled_db = sled::open_database(db_path.clone(), db_config.clone()).await;

    if let Ok(db) = sqlite_db {
        todo!("a");
    } else if let Ok(db) = sled_db {
        todo!();
    }

    Ok(())
}
