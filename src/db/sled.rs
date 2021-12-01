use std::ops::RangeInclusive;

use crate::config::DbConfig;

use super::{ArcTree, Batch, Db, DbError, DbResult, Iter, RangeIter, Tree};

pub fn open_sled<P: AsRef<std::path::Path> + std::fmt::Display>(
    db_path: P,
    db_config: DbConfig,
) -> Result<Box<dyn Db>, String> {
    let result = sled::Config::new()
        .use_compression(true)
        .path(db_path)
        .cache_capacity(db_config.db_cache_limit * 1024 * 1024)
        .mode(
            db_config
                .sled_throughput_at_storage_cost
                .then(|| sled::Mode::HighThroughput)
                .unwrap_or(sled::Mode::LowSpace),
        )
        .open()
        .and_then(|db| db.verify_integrity().map(|_| db));

    match result {
        Ok(db) => Ok(Box::new(db)),
        Err(err) => Err(err.to_string()),
    }
}

impl Db for sled::Db {
    fn open_tree(&self, name: &[u8]) -> DbResult<ArcTree> {
        self.open_tree(name).map_err(Into::into).map(|tree| {
            let box_tree: Box<dyn Tree> = Box::new(tree);
            box_tree.into()
        })
    }
}

impl Tree for sled::Tree {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, DbError> {
        self.get(key)
            .map_err(Into::into)
            .map(|opt| opt.map(|i| i.to_vec()))
    }

    fn insert(&self, key: &[u8], value: &[u8]) -> Result<Option<Vec<u8>>, DbError> {
        self.insert(key, value)
            .map_err(Into::into)
            .map(|opt| opt.map(|i| i.to_vec()))
    }

    fn remove(&self, key: &[u8]) -> Result<Option<Vec<u8>>, DbError> {
        self.remove(key)
            .map_err(Into::into)
            .map(|opt| opt.map(|i| i.to_vec()))
    }

    fn scan_prefix<'a>(&'a self, prefix: &[u8]) -> Iter<'a> {
        Box::new(self.scan_prefix(prefix).map(|res| {
            res.map(|(a, b)| (a.to_vec(), b.to_vec()))
                .map_err(Into::into)
        }))
    }

    fn iter(&self) -> Iter<'_> {
        Box::new(self.iter().map(|res| {
            res.map(|(a, b)| (a.to_vec(), b.to_vec()))
                .map_err(Into::into)
        }))
    }

    fn apply_batch(&self, batch: Batch) -> Result<(), DbError> {
        self.apply_batch(batch.into()).map_err(Into::into)
    }

    fn contains_key(&self, key: &[u8]) -> Result<bool, DbError> {
        self.contains_key(key).map_err(Into::into)
    }

    fn range<'a>(&'a self, range: RangeInclusive<&[u8]>) -> RangeIter<'a> {
        Box::new(self.range(range).map(|res| {
            res.map(|(a, b)| (a.to_vec(), b.to_vec()))
                .map_err(Into::into)
        }))
    }

    fn verify_integrity(&self) -> DbResult<()> {
        self.verify_integrity().map_err(Into::into)
    }
}

impl From<sled::Error> for DbError {
    fn from(inner: sled::Error) -> Self {
        DbError {
            inner: Box::new(inner),
        }
    }
}

impl From<Batch> for sled::Batch {
    fn from(batch: Batch) -> Self {
        let mut sled = sled::Batch::default();
        for (key, value) in batch.inserts {
            match value {
                Some(value) => sled.insert(key, value),
                None => sled.remove(key),
            }
        }
        sled
    }
}
