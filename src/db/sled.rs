use std::ops::RangeInclusive;

use hrpc::common::future::Ready;

use crate::{config::DbConfig, utils::evec::EVec};

use super::{Batch, DbError, DbResult};

type SledFut<T> = Ready<DbResult<T>>;

pub mod shared {
    use hrpc::common::future::ready;

    use super::*;

    pub async fn open_database(db_path: String, db_config: DbConfig) -> DbResult<Db> {
        tokio::task::spawn_blocking(move || -> DbResult<Db> {
            let db = sled::Config::new()
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
                .and_then(|db| db.verify_integrity().map(|_| db))?;

            if db_config.sled_load_to_cache_on_startup {
                for tree_name in db.tree_names() {
                    let tree = db.open_tree(tree_name)?;
                    for res in tree.iter() {
                        res?;
                    }
                }
            }

            Ok(Db { inner: db })
        })
        .await
        .unwrap()
    }

    #[derive(Debug, Clone)]
    pub struct Db {
        inner: sled::Db,
    }

    impl Db {
        pub fn open_tree(&self, name: &[u8]) -> SledFut<Tree> {
            ready(
                self.inner
                    .open_tree(name)
                    .map_err(Into::into)
                    .map(|tree| Tree { inner: tree }),
            )
        }

        pub async fn flush(&self) -> DbResult<usize> {
            self.inner.flush_async().await.map_err(Into::into)
        }
    }

    #[derive(Debug, Clone)]
    pub struct Tree {
        inner: sled::Tree,
    }

    impl Tree {
        pub fn get(&self, key: &[u8]) -> SledFut<Option<EVec>> {
            ready(
                self.inner
                    .get(key)
                    .map_err(Into::into)
                    .map(|opt| opt.map(|i| i.into())),
            )
        }

        pub fn insert(&self, key: &[u8], value: impl Into<sled::IVec>) -> SledFut<Option<EVec>> {
            ready(
                self.inner
                    .insert(key, value)
                    .map_err(Into::into)
                    .map(|opt| opt.map(|i| i.into())),
            )
        }

        pub fn remove(&self, key: &[u8]) -> SledFut<Option<EVec>> {
            ready(
                self.inner
                    .remove(key)
                    .map_err(Into::into)
                    .map(|opt| opt.map(|i| i.into())),
            )
        }

        pub fn scan_prefix<'a>(
            &'a self,
            prefix: &[u8],
        ) -> Ready<impl Iterator<Item = DbResult<(EVec, EVec)>> + 'a> {
            ready(
                self.inner
                    .scan_prefix(prefix)
                    .map(|res| res.map(|(a, b)| (a.into(), b.into())).map_err(Into::into)),
            )
        }

        pub fn iter(&self) -> Ready<impl Iterator<Item = DbResult<(EVec, EVec)>> + '_> {
            ready(
                self.inner
                    .iter()
                    .map(|res| res.map(|(a, b)| (a.into(), b.into())).map_err(Into::into)),
            )
        }

        pub fn apply_batch(&self, batch: Batch) -> SledFut<()> {
            ready(self.inner.apply_batch(batch.into()).map_err(Into::into))
        }

        pub fn contains_key(&self, key: &[u8]) -> SledFut<bool> {
            ready(self.inner.contains_key(key).map_err(Into::into))
        }

        pub fn range<'a>(
            &'a self,
            range: RangeInclusive<&[u8]>,
        ) -> Ready<impl Iterator<Item = DbResult<(EVec, EVec)>> + DoubleEndedIterator + 'a>
        {
            ready(
                self.inner
                    .range(range)
                    .map(|res| res.map(|(a, b)| (a.into(), b.into())).map_err(Into::into)),
            )
        }

        pub fn verify_integrity(&self) -> SledFut<()> {
            ready(self.inner.verify_integrity().map_err(Into::into))
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
