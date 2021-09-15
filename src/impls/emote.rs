use std::convert::TryInto;

use crate::{
    db::{
        self,
        emote::*,
        profile::{make_user_profile_key, USER_PREFIX},
        rkyv_ser, ArcTree, Batch, Db, DbResult,
    },
    impls::{
        chat::{EventContext, EventSub},
        gen_rand_u64,
    },
    ServerError,
};
use harmony_rust_sdk::api::{
    chat::Event,
    emote::{emote_service_server::EmoteService, *},
    exports::hrpc::{server::ServerError as HrpcServerError, Request},
};
use scherzo_derive::*;
use triomphe::Arc;

use super::{
    auth::SessionMap,
    chat::{EventBroadcast, EventSender, PermCheck},
    Dependencies,
};

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
            disable_ratelimits: deps.config.disable_ratelimits,
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

#[harmony_rust_sdk::api::exports::hrpc::async_trait]
impl EmoteService for EmoteServer {
    type Error = ServerError;

    #[rate(20, 5)]
    async fn delete_emote_from_pack(
        &self,
        request: Request<DeleteEmoteFromPackRequest>,
    ) -> Result<DeleteEmoteFromPackResponse, HrpcServerError<Self::Error>> {
        auth!();

        let DeleteEmoteFromPackRequest { pack_id, name } =
            request.into_parts().0.into_message().await??;

        self.emote_tree
            .check_if_emote_pack_owner(pack_id, user_id)?;

        let key = make_emote_pack_emote_key(pack_id, &name);

        emote_remove!(key);

        let equipped_users = self.emote_tree.calculate_users_pack_equipped(pack_id);
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

        Ok(DeleteEmoteFromPackResponse {})
    }

    #[rate(5, 5)]
    async fn delete_emote_pack(
        &self,
        request: Request<DeleteEmotePackRequest>,
    ) -> Result<DeleteEmotePackResponse, HrpcServerError<Self::Error>> {
        auth!();

        let DeleteEmotePackRequest { pack_id } = request.into_parts().0.into_message().await??;

        self.emote_tree
            .check_if_emote_pack_owner(pack_id, user_id)?;

        let key = make_emote_pack_key(pack_id);

        let mut batch = Batch::default();
        batch.remove(key);
        for res in self.emote_tree.inner.scan_prefix(&key) {
            let (key, _) = res.unwrap();
            batch.remove(key);
        }
        self.emote_tree.inner.apply_batch(batch).unwrap();

        self.emote_tree.dequip_emote_pack_logic(user_id, pack_id);

        let equipped_users = self.emote_tree.calculate_users_pack_equipped(pack_id);
        self.send_event_through_chan(
            EventSub::Homeserver,
            stream_event::Event::EmotePackDeleted(EmotePackDeleted { pack_id }),
            None,
            EventContext::new(equipped_users),
        );

        Ok(DeleteEmotePackResponse {})
    }

    #[rate(10, 5)]
    async fn dequip_emote_pack(
        &self,
        request: Request<DequipEmotePackRequest>,
    ) -> Result<DequipEmotePackResponse, HrpcServerError<Self::Error>> {
        auth!();

        let DequipEmotePackRequest { pack_id } = request.into_parts().0.into_message().await??;

        self.emote_tree.dequip_emote_pack_logic(user_id, pack_id);

        self.send_event_through_chan(
            EventSub::Homeserver,
            stream_event::Event::EmotePackDeleted(EmotePackDeleted { pack_id }),
            None,
            EventContext::new(vec![user_id]),
        );

        Ok(DequipEmotePackResponse {})
    }

    #[rate(10, 5)]
    async fn equip_emote_pack(
        &self,
        request: Request<EquipEmotePackRequest>,
    ) -> Result<EquipEmotePackResponse, HrpcServerError<Self::Error>> {
        auth!();

        let EquipEmotePackRequest { pack_id } = request.into_parts().0.into_message().await??;

        let key = make_emote_pack_key(pack_id);
        if let Some(data) = emote_get!(key) {
            let pack = db::deser_emote_pack(data);
            self.emote_tree.equip_emote_pack_logic(user_id, pack_id);
            self.send_event_through_chan(
                EventSub::Homeserver,
                stream_event::Event::EmotePackAdded(EmotePackAdded { pack: Some(pack) }),
                None,
                EventContext::new(vec![user_id]),
            );
        } else {
            return Err(ServerError::EmotePackNotFound.into());
        }

        Ok(EquipEmotePackResponse {})
    }

    #[rate(20, 5)]
    async fn add_emote_to_pack(
        &self,
        request: Request<AddEmoteToPackRequest>,
    ) -> Result<AddEmoteToPackResponse, HrpcServerError<Self::Error>> {
        auth!();

        let AddEmoteToPackRequest { pack_id, emote } =
            request.into_parts().0.into_message().await??;

        if let Some(emote) = emote {
            self.emote_tree
                .check_if_emote_pack_owner(pack_id, user_id)?;

            let emote_key = make_emote_pack_emote_key(pack_id, &emote.name);
            let data = rkyv_ser(&emote);

            emote_insert!(emote_key / data);

            let equipped_users = self.emote_tree.calculate_users_pack_equipped(pack_id);
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

        Ok(AddEmoteToPackResponse {})
    }

    #[rate(10, 5)]
    async fn get_emote_packs(
        &self,
        request: Request<GetEmotePacksRequest>,
    ) -> Result<GetEmotePacksResponse, HrpcServerError<Self::Error>> {
        auth!();

        let prefix = make_equipped_emote_prefix(user_id);
        let equipped_packs = self
            .emote_tree
            .inner
            .scan_prefix(&prefix)
            .filter_map(|res| {
                let (key, _) = res.unwrap();
                (key.len() == make_equipped_emote_key(user_id, 0).len()).then(|| {
                    // Safety: since it will always be 8 bytes left afterwards
                    u64::from_be_bytes(unsafe {
                        key.split_at(prefix.len()).1.try_into().unwrap_unchecked()
                    })
                })
            })
            .collect::<Vec<_>>();

        let packs = self
            .emote_tree
            .inner
            .scan_prefix(EMOTEPACK_PREFIX)
            .filter_map(|res| {
                let (key, val) = res.unwrap();
                let pack_id = (key.len() == make_emote_pack_key(0).len()).then(|| {
                    // Safety: since it will always be 8 bytes left afterwards
                    u64::from_be_bytes(unsafe {
                        key.split_at(EMOTEPACK_PREFIX.len())
                            .1
                            .try_into()
                            .unwrap_unchecked()
                    })
                })?;
                equipped_packs
                    .contains(&pack_id)
                    .then(|| db::deser_emote_pack(val))
            })
            .collect();

        Ok(GetEmotePacksResponse { packs })
    }

    #[rate(20, 5)]
    async fn get_emote_pack_emotes(
        &self,
        request: Request<GetEmotePackEmotesRequest>,
    ) -> Result<GetEmotePackEmotesResponse, HrpcServerError<Self::Error>> {
        auth!();

        let GetEmotePackEmotesRequest { pack_id } = request.into_parts().0.into_message().await??;

        let pack_key = make_emote_pack_key(pack_id);

        if emote_get!(pack_key).is_none() {
            return Err(ServerError::EmotePackNotFound.into());
        }

        let emotes = self
            .emote_tree
            .inner
            .scan_prefix(&pack_key)
            .flat_map(|res| {
                let (key, value) = res.unwrap();
                (key.len() > pack_key.len()).then(|| db::deser_emote(value))
            })
            .collect();

        Ok(GetEmotePackEmotesResponse { emotes })
    }

    #[rate(5, 5)]
    async fn create_emote_pack(
        &self,
        request: Request<CreateEmotePackRequest>,
    ) -> Result<CreateEmotePackResponse, HrpcServerError<Self::Error>> {
        auth!();

        let CreateEmotePackRequest { pack_name } = request.into_parts().0.into_message().await??;

        let pack_id = gen_rand_u64();
        let key = make_emote_pack_key(pack_id);

        let emote_pack = EmotePack {
            pack_id,
            pack_name,
            pack_owner: user_id,
        };
        let data = rkyv_ser(&emote_pack);

        emote_insert!(key / data);

        self.emote_tree.equip_emote_pack_logic(user_id, pack_id);
        self.send_event_through_chan(
            EventSub::Homeserver,
            stream_event::Event::EmotePackAdded(EmotePackAdded {
                pack: Some(emote_pack),
            }),
            None,
            EventContext::new(vec![user_id]),
        );

        Ok(CreateEmotePackResponse { pack_id })
    }
}

#[derive(Clone)]
pub struct EmoteTree {
    pub inner: ArcTree,
}

impl EmoteTree {
    pub fn new(db: &dyn Db) -> DbResult<Self> {
        let inner = db.open_tree(b"emote")?;
        Ok(Self { inner })
    }

    pub fn check_if_emote_pack_owner(
        &self,
        pack_id: u64,
        user_id: u64,
    ) -> Result<EmotePack, ServerError> {
        let key = make_emote_pack_key(pack_id);

        let pack = if let Some(data) = eemote_get!(key) {
            let pack = db::deser_emote_pack(data);

            if pack.pack_owner != user_id {
                return Err(ServerError::NotEmotePackOwner);
            }

            pack
        } else {
            return Err(ServerError::EmotePackNotFound);
        };

        Ok(pack)
    }

    pub fn dequip_emote_pack_logic(&self, user_id: u64, pack_id: u64) {
        let key = make_equipped_emote_key(user_id, pack_id);
        eemote_remove!(key);
    }

    pub fn equip_emote_pack_logic(&self, user_id: u64, pack_id: u64) {
        let key = make_equipped_emote_key(user_id, pack_id);
        eemote_insert!(key / &[]);
    }

    pub fn calculate_users_pack_equipped(&self, pack_id: u64) -> Vec<u64> {
        let mut result = Vec::new();
        for user_id in self.inner.scan_prefix(USER_PREFIX).filter_map(|res| {
            let (key, _) = res.unwrap();
            (key.len() == make_user_profile_key(0).len()).then(|| {
                u64::from_be_bytes(unsafe {
                    key.split_at(USER_PREFIX.len())
                        .1
                        .try_into()
                        .unwrap_unchecked()
                })
            })
        }) {
            let prefix = make_equipped_emote_prefix(user_id);
            if self
                .inner
                .scan_prefix(&prefix)
                .filter_map(|res| {
                    let (key, _) = res.unwrap();
                    (key.len() == make_equipped_emote_key(user_id, 0).len()).then(|| {
                        u64::from_be_bytes(unsafe {
                            key.split_at(prefix.len()).1.try_into().unwrap_unchecked()
                        })
                    })
                })
                .any(|id| id == pack_id)
            {
                result.push(user_id);
            }
        }
        result
    }
}
