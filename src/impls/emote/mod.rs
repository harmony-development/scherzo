use harmony_rust_sdk::api::{
    chat::Event,
    emote::{emote_service_server::EmoteService, *},
};

use super::{
    chat::{EventBroadcast, EventContext, EventSub, PermCheck},
    gen_rand_u64,
    prelude::*,
};

use db::{
    emote::*,
    profile::{make_user_profile_key, USER_PREFIX},
};

pub mod add_emote_to_pack;
pub mod create_emote_pack;
pub mod delete_emote_from_pack;
pub mod delete_emote_pack;
pub mod dequip_emote_pack;
pub mod equip_emote_pack;
pub mod get_emote_pack_emotes;
pub mod get_emote_packs;

#[derive(Clone)]
pub struct EmoteServer {
    disable_ratelimits: bool,
    deps: Arc<Dependencies>,
}

impl EmoteServer {
    pub fn new(deps: Arc<Dependencies>) -> Self {
        Self {
            disable_ratelimits: deps.config.policy.ratelimit.disable,
            deps,
        }
    }

    pub fn batch(mut self) -> Self {
        self.disable_ratelimits = true;
        self
    }

    #[inline(always)]
    fn send_event_through_chan(
        &self,
        sub: EventSub,
        event: stream_event::Event,
        perm_check: Option<PermCheck<'static>>,
        context: EventContext,
    ) {
        let broadcast = EventBroadcast::new(sub, Event::Emote(event), perm_check, context);

        drop(self.deps.chat_event_sender.send(Arc::new(broadcast)));
    }
}

impl EmoteService for EmoteServer {
    impl_unary_handlers! {
        #[rate(7, 5)]
        delete_emote_from_pack, DeleteEmoteFromPackRequest, DeleteEmoteFromPackResponse;
        #[rate(7, 5)]
        add_emote_to_pack, AddEmoteToPackRequest, AddEmoteToPackResponse;
        #[rate(3, 5)]
        delete_emote_pack, DeleteEmotePackRequest, DeleteEmotePackResponse;
        #[rate(3, 5)]
        create_emote_pack, CreateEmotePackRequest, CreateEmotePackResponse;
        #[rate(7, 4)]
        get_emote_pack_emotes, GetEmotePackEmotesRequest, GetEmotePackEmotesResponse;
        #[rate(5, 5)]
        get_emote_packs, GetEmotePacksRequest, GetEmotePacksResponse;
        #[rate(5, 5)]
        equip_emote_pack, EquipEmotePackRequest, EquipEmotePackResponse;
        #[rate(5, 5)]
        dequip_emote_pack, DequipEmotePackRequest, DequipEmotePackResponse;
    }
}

#[derive(Clone)]
pub struct EmoteTree {
    pub inner: Tree,
}

impl EmoteTree {
    impl_db_methods!(inner);

    pub async fn new(db: &Db) -> DbResult<Self> {
        let inner = db.open_tree(b"emote").await?;
        Ok(Self { inner })
    }

    pub async fn check_if_emote_pack_owner(
        &self,
        pack_id: u64,
        user_id: u64,
    ) -> ServerResult<EmotePack> {
        let key = make_emote_pack_key(pack_id);

        let pack = if let Some(data) = self.get(key).await? {
            let pack = db::deser_emote_pack(data);

            if pack.pack_owner != user_id {
                return Err(ServerError::NotEmotePackOwner.into());
            }

            pack
        } else {
            return Err(ServerError::EmotePackNotFound.into());
        };

        Ok(pack)
    }

    pub async fn dequip_emote_pack_logic(&self, user_id: u64, pack_id: u64) -> ServerResult<()> {
        let key = make_equipped_emote_key(user_id, pack_id);
        self.remove(key).await?;
        Ok(())
    }

    pub async fn equip_emote_pack_logic(&self, user_id: u64, pack_id: u64) -> ServerResult<()> {
        let key = make_equipped_emote_key(user_id, pack_id);
        self.insert(key, &[]).await?;
        Ok(())
    }

    pub async fn calculate_users_pack_equipped(&self, pack_id: u64) -> ServerResult<Vec<u64>> {
        let mut result = Vec::new();
        for user_id in
            self.inner
                .scan_prefix(USER_PREFIX)
                .await
                .try_fold(Vec::new(), |mut all, res| {
                    let (key, _) = res.map_err(ServerError::from)?;
                    if key.len() == make_user_profile_key(0).len() {
                        all.push(deser_id(key.split_at(USER_PREFIX.len()).1));
                    }
                    ServerResult::Ok(all)
                })?
        {
            let prefix = make_equipped_emote_prefix(user_id);
            let mut has = false;
            for res in self.inner.scan_prefix(&prefix).await {
                let (key, _) = res.map_err(ServerError::from)?;
                if key.len() == make_equipped_emote_key(user_id, 0).len() {
                    let id = deser_id(key.split_at(prefix.len()).1);
                    if id == pack_id {
                        has = true;
                        break;
                    }
                }
            }
            if has {
                result.push(user_id);
            }
        }
        Ok(result)
    }
}
