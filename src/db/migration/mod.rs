use std::future::Future;

use hrpc::server::gen_prelude::BoxFuture;
use tracing::Instrument;

use crate::db;

use super::{Db, DbResult};

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
