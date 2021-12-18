use std::{collections::HashMap, path::PathBuf};

use hrpc::BoxError;
use itertools::Itertools;
use scherzo::{
    db::{self, sled::shared as sled, sqlite::shared as sqlite, Batch},
    utils::evec::EVec,
};

fn main() -> Result<(), BoxError> {
    let rt = tokio::runtime::Runtime::new()?;
    let _rt_guard = rt.enter();

    let mut args = std::env::args().collect_vec();

    if args.len() < 4 {
        println!(
            "usage: {} <db path> <db type> <db target type>\nexample: {} ./db sled sqlite",
            args[0], args[0]
        );
        std::process::exit(1);
    }

    args.reverse();
    args.pop();

    let db_path = args.pop().expect("expected db path");
    let db_type = args.pop().expect("expected db type to migrate to");
    let db_target = args.pop().expect("expected db type to migrate to");
    let db_config = scherzo::config::DbConfig::default();

    let mut db_backup_path = PathBuf::from(db_path.clone());
    db_backup_path.set_file_name(format!(
        "{}_migrate_backup",
        db_backup_path.file_name().unwrap().to_string_lossy()
    ));

    std::fs::rename(&db_path, &db_backup_path)?;

    let db_backup_path = db_backup_path.into_os_string().into_string().unwrap();

    rt.block_on(async move {
        let src_db = match db_type.as_str() {
            "sled" => Db::Sled(sled::open_database(db_backup_path, db_config.clone()).await?),
            "sqlite" => Db::Sqlite(sqlite::open_database(db_backup_path, db_config.clone()).await?),
            _ => panic!("no such db"),
        };

        let target_db = match db_target.as_str() {
            "sled" => Db::Sled(sled::open_database(db_path, db_config).await?),
            "sqlite" => Db::Sqlite(sqlite::open_database(db_path, db_config).await?),
            _ => panic!("no such db"),
        };

        let vals = src_db.iter().await?;
        target_db.insert(vals).await?;
        target_db.flush().await?;

        Ok(())
    })
}

type ValueMap = HashMap<&'static [u8], Vec<(EVec, EVec)>, ahash::RandomState>;

enum Db {
    Sled(sled::Db),
    Sqlite(sqlite::Db),
}

impl Db {
    async fn iter(&self) -> Result<ValueMap, BoxError> {
        let mut treemap = HashMap::with_hasher(ahash::RandomState::new());
        for name in db::TREES {
            match self {
                Self::Sled(db) => {
                    let tree = db.open_tree(name).await?;
                    treemap.insert(
                        name,
                        tree.iter().await.fold_ok(Vec::new(), |mut all, item| {
                            all.push(item);
                            all
                        })?,
                    );
                }
                Self::Sqlite(db) => {
                    let tree = db.open_tree(name).await?;
                    treemap.insert(
                        name,
                        tree.iter().await.fold_ok(Vec::new(), |mut all, item| {
                            all.push(item);
                            all
                        })?,
                    );
                }
            }
        }
        Ok(treemap)
    }

    async fn insert(&self, mut values: ValueMap) -> Result<(), BoxError> {
        for name in db::TREES {
            match self {
                Self::Sled(db) => {
                    let tree = db.open_tree(name).await?;
                    let mut batch = Batch::default();
                    for (k, v) in values.remove(name).expect("no such tree") {
                        batch.insert(k, v);
                    }
                    tree.apply_batch(batch).await?;
                }
                Self::Sqlite(db) => {
                    let tree = db.open_tree(name).await?;
                    let mut batch = Batch::default();
                    for (k, v) in values.remove(name).expect("no such tree") {
                        batch.insert(k, v);
                    }
                    tree.apply_batch(batch).await?;
                }
            }
        }
        Ok(())
    }

    async fn flush(&self) -> Result<(), BoxError> {
        match self {
            Self::Sled(db) => db.flush().await?,
            Self::Sqlite(db) => db.flush().await?,
        }
        Ok(())
    }
}
