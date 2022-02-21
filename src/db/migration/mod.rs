use std::future::Future;

use anyhow::Context;
use bytecheck::CheckBytes;
use hrpc::server::gen_prelude::BoxFuture;
use rkyv::{
    de::deserializers::SharedDeserializeMap, ser::serializers::AllocSerializer,
    validation::validators::DefaultValidator, Archive, Deserialize, Serialize,
};
use tracing::Instrument;

use crate::db;

use super::{rkyv_ser, Batch, Db, DbResult, Tree};

mod add_account_kind;
mod add_next_msg_ids;
mod initial_db_version;
mod remove_log_chan_id_from_admin_keys;
mod timestamps_are_milliseconds;

type Migration = for<'a> fn(&'a Db) -> BoxFuture<'a, DbResult<()>>;

pub const MIGRATIONS: [Migration; 5] = [
    initial_db_version::migrate,
    add_next_msg_ids::migrate,
    remove_log_chan_id_from_admin_keys::migrate,
    add_account_kind::migrate,
    timestamps_are_milliseconds::migrate,
];

pub async fn get_db_version(db: &Db) -> DbResult<(usize, bool)> {
    let version_tree = db.open_tree(b"version").await?;
    let version = version_tree
        .get(b"version")
        .await?
        .and_then(|raw| Some(usize::from_be_bytes(raw.try_into().ok()?)))
        .unwrap_or(0);
    Ok((version, version < MIGRATIONS.len()))
}

pub fn apply_migrations(
    db: &Db,
    current_version: usize,
) -> impl Future<Output = DbResult<()>> + '_ {
    let fut = async move {
        if current_version == 0 {
            initial_db_version::migrate(db).await
        } else {
            for (version, migration) in MIGRATIONS.into_iter().enumerate().skip(current_version) {
                tracing::warn!(
                    "migrating database from version {} to {}",
                    version,
                    version + 1
                );
                migration(db).await?;
                increment_db_version(db).await?;
            }
            Ok(())
        }
    };
    fut.instrument(
        tracing::info_span!("apply_migrations", before_migration_version = %current_version),
    )
}

async fn increment_db_version(db: &Db) -> DbResult<()> {
    let version_tree = db.open_tree(b"version").await?;
    if let Some(version) = version_tree
        .get(b"version")
        .await?
        .and_then(|raw| Some(usize::from_be_bytes(raw.try_into().ok()?)))
    {
        let new_version = version + 1;
        tracing::info!(
            "migrated database from version {} to {}",
            version,
            new_version
        );
        version_tree
            .insert(b"version", &new_version.to_be_bytes())
            .await?;
    }
    Ok(())
}

async fn migrate_type<From, To, F>(tree: &Tree, prefix: &[u8], mut migrate: F) -> DbResult<()>
where
    From: Archive,
    To: Archive + Serialize<AllocSerializer<1024>>,
    From::Archived:
        for<'a> CheckBytes<DefaultValidator<'a>> + Deserialize<From, SharedDeserializeMap>,
    To::Archived: for<'a> CheckBytes<DefaultValidator<'a>>,
    F: FnMut(From) -> To,
{
    let mut batch = Batch::default();
    for res in tree.scan_prefix(prefix).await {
        let (key, val) = res?;
        let old = rkyv::from_bytes::<From>(&val);
        if let Ok(old) = old {
            let new_val = migrate(old);
            let new_val = rkyv_ser(&new_val);
            batch.insert(key, new_val);
        } else if rkyv::check_archived_root::<To>(&val).is_ok() {
            // if it's new, then its already fine
            continue;
        } else {
            old.map_err(|err| anyhow::anyhow!(err.to_string()))
                .with_context(|| format!("invalid state on key: {:?}", key.as_ref()))?;
        }
    }
    tree.apply_batch(batch).await?;
    Ok(())
}
