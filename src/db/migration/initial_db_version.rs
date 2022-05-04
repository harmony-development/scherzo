use super::*;

define_migration!(|db| {
    let version_tree = db.open_tree(b"version").await?;
    if !version_tree.contains_key(b"version").await? {
        version_tree
            .insert(b"version", &MIGRATIONS.len().to_be_bytes())
            .await?;
    }
    Ok(())
});
