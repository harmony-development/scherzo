use std::ops::RangeInclusive;

use super::{Batch, Db, DbError, DbResult, IVec, Iter, RangeIter, Tree};

impl Db for sled::Db {
    fn open_tree(&self, name: &[u8]) -> DbResult<std::sync::Arc<dyn Tree>> {
        self.open_tree(name).map_err(Into::into).map(|tree| {
            let box_tree: Box<dyn Tree> = Box::new(tree);
            box_tree.into()
        })
    }
}

impl Tree for sled::Tree {
    fn get(&self, key: &[u8]) -> Result<Option<IVec>, DbError> {
        self.get(key)
            .map_err(Into::into)
            .map(|opt| opt.map(Into::into))
    }

    fn insert(&self, key: &[u8], value: &[u8]) -> Result<Option<IVec>, DbError> {
        self.insert(key, value)
            .map_err(Into::into)
            .map(|opt| opt.map(Into::into))
    }

    fn remove(&self, key: &[u8]) -> Result<Option<IVec>, DbError> {
        self.remove(key)
            .map_err(Into::into)
            .map(|opt| opt.map(Into::into))
    }

    fn scan_prefix<'a>(&'a self, prefix: &[u8]) -> Iter<'a> {
        Box::new(
            self.scan_prefix(prefix)
                .map(|res| res.map(|(a, b)| (a.into(), b.into())).map_err(Into::into)),
        )
    }

    fn apply_batch(&self, batch: Batch) -> Result<(), DbError> {
        self.apply_batch(batch.into()).map_err(Into::into)
    }

    fn contains_key(&self, key: &[u8]) -> Result<bool, DbError> {
        self.contains_key(key).map_err(Into::into)
    }

    fn range<'a>(&'a self, range: RangeInclusive<&[u8]>) -> RangeIter<'a> {
        Box::new(
            self.range(range)
                .map(|res| res.map(|(a, b)| (a.into(), b.into())).map_err(Into::into)),
        )
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
