use std::fmt::Display;

use super::*;

use db::{profile::USER_PREFIX, rkyv_ser, Batch, DbError};
use hrpc::box_error;

pub(super) fn migrate(db: &Db) -> BoxFuture<'_, DbResult<()>> {
    let fut = async move {
        let profile_tree = db.open_tree(b"profile").await?;
        let mut batch = Batch::default();
        for res in profile_tree.scan_prefix(USER_PREFIX).await {
            let (key, val) = res?;
            let old_profile = rkyv::from_bytes::<profile::Profile>(&val);
            if let Ok(old_profile) = &old_profile {
                let new_val = rkyv_ser(old_profile);
                batch.insert(key, new_val);
            } else if rkyv::check_archived_root::<crate::api::profile::Profile>(&val).is_ok() {
                // if it's new, then its already fine
                continue;
            } else {
                old_profile.map_err(|err| DbError {
                    inner: box_error(AnyhowError(anyhow::anyhow!(
                        "profile with key {} has invalid state: {}",
                        String::from_utf8_lossy(key.as_ref()),
                        err
                    ))),
                })?;
            }
        }
        profile_tree.apply_batch(batch).await?;
        Ok(())
    };

    Box::pin(fut)
}

scherzo_derive::define_proto_mod!(before_account_kind, profile);
scherzo_derive::define_proto_mod!(before_account_kind, harmonytypes);

#[derive(Debug)]
struct AnyhowError(anyhow::Error);

impl Display for AnyhowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl std::error::Error for AnyhowError {}
