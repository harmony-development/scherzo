use std::convert::TryInto;

use super::{Db, DbResult};

type Migration = fn(&dyn Db) -> DbResult<()>;

pub const MIGRATIONS: [Migration; 1] = [initial_db_version];

pub fn get_db_version(db: &dyn Db) -> DbResult<(usize, bool)> {
    let version_tree = db.open_tree(b"version")?;
    let version = version_tree
        .get(b"version")?
        .and_then(|raw| Some(usize::from_be_bytes(raw.try_into().ok()?)))
        .unwrap_or(0);
    Ok((version, version < MIGRATIONS.len()))
}

pub fn apply_migrations(db: &dyn Db, current_version: usize) -> DbResult<()> {
    for migration in std::array::IntoIter::new(MIGRATIONS).skip(current_version) {
        migration(db)?;
        increment_db_version(db)?;
    }
    Ok(())
}

fn increment_db_version(db: &dyn Db) -> DbResult<()> {
    let version_tree = db.open_tree(b"version")?;
    if let Some(version) = version_tree
        .get(b"version")?
        .and_then(|raw| Some(usize::from_be_bytes(raw.try_into().ok()?)))
    {
        let new_version = version + 1;
        tracing::warn!(
            "migrating database from version {} to {}",
            version,
            new_version
        );
        version_tree.insert(b"version", &new_version.to_be_bytes())?;
    }
    Ok(())
}

fn initial_db_version(db: &dyn Db) -> DbResult<()> {
    let version_tree = db.open_tree(b"version")?;
    if !version_tree.contains_key(b"version")? {
        version_tree.insert(b"version", &0_usize.to_be_bytes())?;
    }
    Ok(())
}
