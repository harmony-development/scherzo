use harmony_rust_sdk::api::exports::{hrpc::encode_protobuf_message, prost::Message};

use std::{convert::TryInto, mem::size_of};

use crate::db::{chat::make_msg_key, Batch};

use super::{Db, DbResult};

type Migration = fn(&dyn Db) -> DbResult<()>;

pub const MIGRATIONS: [Migration; 2] = [initial_db_version, migrate_contents];

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

fn migrate_contents(db: &dyn Db) -> DbResult<()> {
    let tree = db.open_tree(b"chat")?;
    const LEN: usize = make_msg_key(0, 0, 0).len();

    let messages = tree
        .iter()
        .filter_map(|res| {
            let (key, val) = res.unwrap();
            (key.len() == LEN)
                .then(|| {
                    let (gid_raw, rest) = key.split_at(size_of::<u64>());
                    if rest[0] != 8 {
                        return None;
                    }
                    let gid = u64::from_be_bytes(gid_raw.try_into().ok()?);
                    let (cid_raw, rest) = rest[1..].split_at(size_of::<u64>());
                    if rest[0] != 9 {
                        return None;
                    }
                    let cid = u64::from_be_bytes(cid_raw.try_into().ok()?);
                    let mid = u64::from_be_bytes(rest[1..].try_into().ok()?);

                    let msg = sdk_migration_two::api::harmonytypes::Message::decode(val.as_slice())
                        .ok()?;

                    Some((make_msg_key(gid, cid, mid), msg))
                })
                .flatten()
        });

    let mut batch = Batch::default();
    for (key, msg) in messages {
        let val = encode_protobuf_message(msg);
        batch.insert(&key, val.as_ref());
    }
    tree.apply_batch(batch)?;

    Ok(())
}
