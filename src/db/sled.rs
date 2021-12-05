use std::ops::RangeInclusive;

use crate::{config::DbConfig, utils::evec::EVec};

use super::{Batch, DbError, DbResult};

pub mod shared {
    use super::*;

    pub fn open_database<P: AsRef<std::path::Path> + std::fmt::Display>(
        db_path: P,
        db_config: DbConfig,
    ) -> Result<Db, String> {
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
            Ok(db) => Ok(Db { inner: db }),
            Err(err) => Err(err.to_string()),
        }
    }

    #[derive(Debug, Clone)]
    pub struct Db {
        inner: sled::Db,
    }

    impl Db {
        pub fn open_tree(&self, name: &[u8]) -> DbResult<Tree> {
            self.inner
                .open_tree(name)
                .map_err(Into::into)
                .map(|tree| Tree { inner: tree })
        }
    }

    #[derive(Debug, Clone)]
    pub struct Tree {
        inner: sled::Tree,
    }

    impl Tree {
        pub fn get(&self, key: &[u8]) -> Result<Option<EVec>, DbError> {
            self.inner
                .get(key)
                .map_err(Into::into)
                .map(|opt| opt.map(|i| i.into()))
        }

        pub fn insert(&self, key: &[u8], value: &[u8]) -> Result<Option<EVec>, DbError> {
            self.inner
                .insert(key, value)
                .map_err(Into::into)
                .map(|opt| opt.map(|i| i.into()))
        }

        pub fn remove(&self, key: &[u8]) -> Result<Option<EVec>, DbError> {
            self.inner
                .remove(key)
                .map_err(Into::into)
                .map(|opt| opt.map(|i| i.into()))
        }

        pub fn scan_prefix<'a>(
            &'a self,
            prefix: &[u8],
        ) -> impl Iterator<Item = DbResult<(EVec, EVec)>> + 'a {
            self.inner
                .scan_prefix(prefix)
                .map(|res| res.map(|(a, b)| (a.into(), b.into())).map_err(Into::into))
        }

        pub fn iter(&self) -> impl Iterator<Item = DbResult<(EVec, EVec)>> + '_ {
            self.inner
                .iter()
                .map(|res| res.map(|(a, b)| (a.into(), b.into())).map_err(Into::into))
        }

        pub fn apply_batch(&self, batch: Batch) -> Result<(), DbError> {
            self.inner.apply_batch(batch.into()).map_err(Into::into)
        }

        pub fn contains_key(&self, key: &[u8]) -> Result<bool, DbError> {
            self.inner.contains_key(key).map_err(Into::into)
        }

        pub fn range<'a>(
            &'a self,
            range: RangeInclusive<&[u8]>,
        ) -> impl Iterator<Item = DbResult<(EVec, EVec)>> + DoubleEndedIterator + 'a {
            self.inner
                .range(range)
                .map(|res| res.map(|(a, b)| (a.into(), b.into())).map_err(Into::into))
        }

        pub fn verify_integrity(&self) -> DbResult<()> {
            self.inner.verify_integrity().map_err(Into::into)
        }
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
