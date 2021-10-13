use harmony_rust_sdk::api::{
    chat::Event,
    emote::{emote_service_server::EmoteService, *},
};

use super::{
    chat::{EventBroadcast, EventContext, EventSender, EventSub, PermCheck},
    gen_rand_u64,
    prelude::*,
};

use db::{
    emote::*,
    profile::{make_user_profile_key, USER_PREFIX},
};

#[derive(Clone)]
pub struct EmoteServer {
    emote_tree: EmoteTree,
    valid_sessions: SessionMap,
    pub broadcast_send: EventSender,
    disable_ratelimits: bool,
}

impl EmoteServer {
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            emote_tree: deps.emote_tree.clone(),
            valid_sessions: deps.valid_sessions.clone(),
            broadcast_send: deps.chat_event_sender.clone(),
            disable_ratelimits: deps.config.policy.disable_ratelimits,
        }
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

        drop(self.broadcast_send.send(Arc::new(broadcast)));
    }
}

#[async_trait]
impl EmoteService for EmoteServer {
    #[rate(20, 5)]
    async fn delete_emote_from_pack(
        &mut self,
        request: Request<DeleteEmoteFromPackRequest>,
    ) -> ServerResult<Response<DeleteEmoteFromPackResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let DeleteEmoteFromPackRequest { pack_id, name } = request.into_message().await?;

        self.emote_tree
            .check_if_emote_pack_owner(pack_id, user_id)?;

        let key = make_emote_pack_emote_key(pack_id, &name);

        self.emote_tree.remove(key)?;

        let equipped_users = self.emote_tree.calculate_users_pack_equipped(pack_id)?;
        self.send_event_through_chan(
            EventSub::Homeserver,
            stream_event::Event::EmotePackEmotesUpdated(EmotePackEmotesUpdated {
                pack_id,
                added_emotes: Vec::new(),
                deleted_emotes: vec![name],
            }),
            None,
            EventContext::new(equipped_users),
        );

        Ok((DeleteEmoteFromPackResponse {}).into_response())
    }

    #[rate(5, 5)]
    async fn delete_emote_pack(
        &mut self,
        request: Request<DeleteEmotePackRequest>,
    ) -> ServerResult<Response<DeleteEmotePackResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let DeleteEmotePackRequest { pack_id } = request.into_message().await?;

        self.emote_tree
            .check_if_emote_pack_owner(pack_id, user_id)?;

        let key = make_emote_pack_key(pack_id);

        let mut batch = Batch::default();
        batch.remove(key);
        for res in self.emote_tree.inner.scan_prefix(&key) {
            let (key, _) = res.map_err(ServerError::DbError)?;
            batch.remove(key);
        }
        self.emote_tree
            .inner
            .apply_batch(batch)
            .map_err(ServerError::DbError)?;

        self.emote_tree.dequip_emote_pack_logic(user_id, pack_id)?;

        let equipped_users = self.emote_tree.calculate_users_pack_equipped(pack_id)?;
        self.send_event_through_chan(
            EventSub::Homeserver,
            stream_event::Event::EmotePackDeleted(EmotePackDeleted { pack_id }),
            None,
            EventContext::new(equipped_users),
        );

        Ok((DeleteEmotePackResponse {}).into_response())
    }

    #[rate(10, 5)]
    async fn dequip_emote_pack(
        &mut self,
        request: Request<DequipEmotePackRequest>,
    ) -> ServerResult<Response<DequipEmotePackResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let DequipEmotePackRequest { pack_id } = request.into_message().await?;

        self.emote_tree.dequip_emote_pack_logic(user_id, pack_id)?;

        self.send_event_through_chan(
            EventSub::Homeserver,
            stream_event::Event::EmotePackDeleted(EmotePackDeleted { pack_id }),
            None,
            EventContext::new(vec![user_id]),
        );

        Ok((DequipEmotePackResponse {}).into_response())
    }

    #[rate(10, 5)]
    async fn equip_emote_pack(
        &mut self,
        request: Request<EquipEmotePackRequest>,
    ) -> ServerResult<Response<EquipEmotePackResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let EquipEmotePackRequest { pack_id } = request.into_message().await?;

        let key = make_emote_pack_key(pack_id);
        if let Some(data) = self.emote_tree.get(key)? {
            let pack = db::deser_emote_pack(data);
            self.emote_tree.equip_emote_pack_logic(user_id, pack_id)?;
            self.send_event_through_chan(
                EventSub::Homeserver,
                stream_event::Event::EmotePackAdded(EmotePackAdded { pack: Some(pack) }),
                None,
                EventContext::new(vec![user_id]),
            );
        } else {
            return Err(ServerError::EmotePackNotFound.into());
        }

        Ok((EquipEmotePackResponse {}).into_response())
    }

    #[rate(20, 5)]
    async fn add_emote_to_pack(
        &mut self,
        request: Request<AddEmoteToPackRequest>,
    ) -> ServerResult<Response<AddEmoteToPackResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let AddEmoteToPackRequest { pack_id, emote } = request.into_message().await?;

        if let Some(emote) = emote {
            self.emote_tree
                .check_if_emote_pack_owner(pack_id, user_id)?;

            let emote_key = make_emote_pack_emote_key(pack_id, &emote.name);
            let data = rkyv_ser(&emote);

            self.emote_tree.insert(emote_key, data)?;

            let equipped_users = self.emote_tree.calculate_users_pack_equipped(pack_id)?;
            self.send_event_through_chan(
                EventSub::Homeserver,
                stream_event::Event::EmotePackEmotesUpdated(EmotePackEmotesUpdated {
                    pack_id,
                    added_emotes: vec![emote],
                    deleted_emotes: Vec::new(),
                }),
                None,
                EventContext::new(equipped_users),
            );
        }

        Ok((AddEmoteToPackResponse {}).into_response())
    }

    #[rate(10, 5)]
    async fn get_emote_packs(
        &mut self,
        request: Request<GetEmotePacksRequest>,
    ) -> ServerResult<Response<GetEmotePacksResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let prefix = make_equipped_emote_prefix(user_id);
        let equipped_packs =
            self.emote_tree
                .inner
                .scan_prefix(&prefix)
                .try_fold(Vec::new(), |mut all, res| {
                    let (key, _) = res.map_err(ServerError::from)?;
                    if key.len() == make_equipped_emote_key(user_id, 0).len() {
                        let pack_id =
                        // Safety: since it will always be 8 bytes left afterwards
                        u64::from_be_bytes(unsafe {
                            key.split_at(prefix.len()).1.try_into().unwrap_unchecked()
                        });
                        all.push(pack_id);
                    }
                    ServerResult::Ok(all)
                })?;

        let packs = self
            .emote_tree
            .inner
            .scan_prefix(EMOTEPACK_PREFIX)
            .try_fold(Vec::new(), |mut all, res| {
                let (key, val) = res.map_err(ServerError::from)?;
                if key.len() == make_emote_pack_key(0).len() {
                    let pack_id =
                        // Safety: since it will always be 8 bytes left afterwards
                        u64::from_be_bytes(unsafe {
                            key.split_at(EMOTEPACK_PREFIX.len())
                                .1
                                .try_into()
                                .unwrap_unchecked()
                        });
                    if equipped_packs.contains(&pack_id) {
                        all.push(db::deser_emote_pack(val));
                    }
                }
                ServerResult::Ok(all)
            })?;

        Ok((GetEmotePacksResponse { packs }).into_response())
    }

    #[rate(20, 5)]
    async fn get_emote_pack_emotes(
        &mut self,
        request: Request<GetEmotePackEmotesRequest>,
    ) -> ServerResult<Response<GetEmotePackEmotesResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let GetEmotePackEmotesRequest { pack_id } = request.into_message().await?;

        let pack_key = make_emote_pack_key(pack_id);

        if self.emote_tree.get(pack_key)?.is_none() {
            return Err(ServerError::EmotePackNotFound.into());
        }

        let emotes =
            self.emote_tree
                .inner
                .scan_prefix(&pack_key)
                .try_fold(Vec::new(), |mut all, res| {
                    let (key, value) = res.map_err(ServerError::from)?;
                    if key.len() > pack_key.len() {
                        all.push(db::deser_emote(value));
                    }
                    ServerResult::Ok(all)
                })?;

        Ok((GetEmotePackEmotesResponse { emotes }).into_response())
    }

    #[rate(5, 5)]
    async fn create_emote_pack(
        &mut self,
        request: Request<CreateEmotePackRequest>,
    ) -> ServerResult<Response<CreateEmotePackResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let CreateEmotePackRequest { pack_name } = request.into_message().await?;

        let pack_id = gen_rand_u64();
        let key = make_emote_pack_key(pack_id);

        let emote_pack = EmotePack {
            pack_id,
            pack_name,
            pack_owner: user_id,
        };
        let data = rkyv_ser(&emote_pack);

        self.emote_tree.insert(key, data)?;

        self.emote_tree.equip_emote_pack_logic(user_id, pack_id)?;
        self.send_event_through_chan(
            EventSub::Homeserver,
            stream_event::Event::EmotePackAdded(EmotePackAdded {
                pack: Some(emote_pack),
            }),
            None,
            EventContext::new(vec![user_id]),
        );

        Ok((CreateEmotePackResponse { pack_id }).into_response())
    }
}

#[derive(Clone)]
pub struct EmoteTree {
    pub inner: ArcTree,
}

impl EmoteTree {
    impl_db_methods!(inner);

    pub fn new(db: &dyn Db) -> DbResult<Self> {
        let inner = db.open_tree(b"emote")?;
        Ok(Self { inner })
    }

    pub fn check_if_emote_pack_owner(&self, pack_id: u64, user_id: u64) -> ServerResult<EmotePack> {
        let key = make_emote_pack_key(pack_id);

        let pack = if let Some(data) = self.get(key)? {
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

    pub fn dequip_emote_pack_logic(&self, user_id: u64, pack_id: u64) -> ServerResult<()> {
        let key = make_equipped_emote_key(user_id, pack_id);
        self.remove(key)?;
        Ok(())
    }

    pub fn equip_emote_pack_logic(&self, user_id: u64, pack_id: u64) -> ServerResult<()> {
        let key = make_equipped_emote_key(user_id, pack_id);
        self.insert(key, &[])?;
        Ok(())
    }

    pub fn calculate_users_pack_equipped(&self, pack_id: u64) -> ServerResult<Vec<u64>> {
        let mut result = Vec::new();
        for user_id in
            self.inner
                .scan_prefix(USER_PREFIX)
                .try_fold(Vec::new(), |mut all, res| {
                    let (key, _) = res.map_err(ServerError::from)?;
                    if key.len() == make_user_profile_key(0).len() {
                        all.push(u64::from_be_bytes(unsafe {
                            key.split_at(USER_PREFIX.len())
                                .1
                                .try_into()
                                .unwrap_unchecked()
                        }));
                    }
                    ServerResult::Ok(all)
                })?
        {
            let prefix = make_equipped_emote_prefix(user_id);
            let mut has = false;
            for res in self.inner.scan_prefix(&prefix) {
                let (key, _) = res.map_err(ServerError::from)?;
                if key.len() == make_equipped_emote_key(user_id, 0).len() {
                    let id = u64::from_be_bytes(unsafe {
                        key.split_at(prefix.len()).1.try_into().unwrap_unchecked()
                    });
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
