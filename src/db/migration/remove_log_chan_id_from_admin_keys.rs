use std::mem::size_of;

use crate::db::chat::ADMIN_GUILD_KEY;

use super::*;

define_migration!(|db| {
    let chat_tree = db.open_tree(b"chat").await?;

    if let Some(raw) = chat_tree.get(ADMIN_GUILD_KEY).await? {
        let (gid_raw, rest) = raw.split_at(size_of::<u64>());
        let guild_id = unsafe { u64::from_be_bytes(gid_raw.try_into().unwrap_unchecked()) };
        let (_, cmd_raw) = rest.split_at(size_of::<u64>());
        let cmd_id = unsafe { u64::from_be_bytes(cmd_raw.try_into().unwrap_unchecked()) };

        let new_raw = [guild_id.to_be_bytes(), cmd_id.to_be_bytes()].concat();

        chat_tree.insert(ADMIN_GUILD_KEY, new_raw).await?;
    }

    Ok(())
});
