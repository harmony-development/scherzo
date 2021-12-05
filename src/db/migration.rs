use std::future::Future;

use hrpc::server::gen_prelude::BoxFuture;
use tracing::Instrument;

use crate::db::{deser_chan, Batch};

use super::{Db, DbResult};

type Migration = for<'a> fn(&'a Db) -> BoxFuture<'a, DbResult<()>>;

pub const MIGRATIONS: [Migration; 2] = [initial_db_version, add_next_msg_ids];

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
            initial_db_version(db).await
        } else {
            for (version, migration) in std::array::IntoIter::new(MIGRATIONS)
                .enumerate()
                .skip(current_version)
            {
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

fn initial_db_version(db: &Db) -> BoxFuture<'_, DbResult<()>> {
    let fut = async move {
        let version_tree = db.open_tree(b"version").await?;
        if !version_tree.contains_key(b"version").await? {
            version_tree
                .insert(b"version", &MIGRATIONS.len().to_be_bytes())
                .await?;
        }
        Ok(())
    };

    Box::pin(fut)
}

fn add_next_msg_ids(db: &Db) -> BoxFuture<'_, DbResult<()>> {
    const CHAN_KEY_LEN: usize = super::chat::make_chan_key(0, 0).len();

    let fut = async move {
        let chat_tree = db.open_tree(b"chat").await?;

        let mut batch = Batch::default();
        for res in chat_tree.iter().await {
            let (key, val) = res?;
            let mut key: Vec<u8> = key.into();

            if key.len() == CHAN_KEY_LEN && key[8] == 8 {
                deser_chan(val);
                key.push(9);

                let id = chat_tree
                    .scan_prefix(&key)
                    .await
                    .last()
                    // Ensure that the first message ID is always 1!
                    // otherwise get message id
                    .map_or(Ok(1), |res| {
                        res.map(|res| {
                            u64::from_be_bytes(
                                res.0
                                    .split_at(key.len())
                                    .1
                                    .try_into()
                                    .expect("failed to convert to u64 id"),
                            )
                        })
                    })?;

                key.pop();
                key.push(7);

                batch.insert(key, id.to_be_bytes());
            }
        }
        chat_tree.apply_batch(batch).await
    };

    Box::pin(fut)
}
