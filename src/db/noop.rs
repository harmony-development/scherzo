use std::ops::RangeInclusive;

use super::{ArcTree, Batch, Db, DbResult, Iter, RangeIter, Tree};

pub struct NoopDb;

pub struct NoopTree;

impl Db for NoopDb {
    fn open_tree(&self, _: &[u8]) -> DbResult<ArcTree> {
        let tree = NoopTree;
        let box_tree: Box<dyn Tree> = Box::new(tree);
        Ok(box_tree.into())
    }
}

impl Tree for NoopTree {
    fn get(&self, _: &[u8]) -> DbResult<Option<Vec<u8>>> {
        Ok(None)
    }

    fn insert(&self, _: &[u8], _: &[u8]) -> DbResult<Option<Vec<u8>>> {
        Ok(None)
    }

    fn remove(&self, _: &[u8]) -> DbResult<Option<Vec<u8>>> {
        Ok(None)
    }

    fn scan_prefix<'a>(&'a self, _: &[u8]) -> Iter<'a> {
        Box::new(std::iter::empty())
    }

    fn apply_batch(&self, _: Batch) -> DbResult<()> {
        Ok(())
    }

    fn contains_key(&self, _: &[u8]) -> DbResult<bool> {
        Ok(false)
    }

    fn range<'a>(&'a self, _: RangeInclusive<&[u8]>) -> RangeIter<'a> {
        Box::new(std::iter::empty())
    }

    fn verify_integrity(&self) -> DbResult<()> {
        Ok(())
    }
}
