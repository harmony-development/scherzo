use super::*;

pub(super) fn migrate(db: &Db) -> BoxFuture<'_, DbResult<()>> {
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
