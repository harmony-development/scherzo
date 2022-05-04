use super::*;

use db::{chat::make_chan_key, deser_chan, Batch};

const CHAN_KEY_LEN: usize = make_chan_key(0, 0).len();

define_migration!(|db| {
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
});
