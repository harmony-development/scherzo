use super::*;

use db::{
    rkyv_ser, Batch
};
use harmony_rust_sdk::api::chat::Message as HarmonyMessage;

pub(super) fn migrate(db: &Db) -> BoxFuture<'_, DbResult<()>> {
    let fut = async move {
        let chat_tree = db.open_tree(b"chat").await?;
        let mut batch = Batch::default();
        let pfix = [];

        for res in chat_tree.scan_prefix(&pfix).await {
            let (key, val) = res?;

            let message = rkyv::from_bytes::<HarmonyMessage>(&val);
            let Ok(mut msg) = message else {
                continue;
            };

            msg.created_at *= 1000;
            msg.edited_at = msg.edited_at.map(|x| x * 1000);

            batch.insert(key, rkyv_ser(&msg));
        }

        chat_tree.apply_batch(batch).await?;
        Ok(())
    };

    Box::pin(fut)
}
