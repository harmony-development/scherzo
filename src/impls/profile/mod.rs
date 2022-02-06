use super::{
    chat::{EventBroadcast, EventContext, EventSub, PermCheck},
    prelude::*,
};

use crate::api::{
    chat::Event,
    profile::{profile_service_server::ProfileService, *},
};
use db::profile::*;

pub mod get_app_data;
pub mod get_profile;
pub mod set_app_data;
pub mod update_profile;

#[derive(Clone)]
pub struct ProfileServer {
    disable_ratelimits: bool,
    deps: Arc<Dependencies>,
}

impl ProfileServer {
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
        let broadcast = EventBroadcast::new(sub, Event::Profile(event), perm_check, context);

        drop(self.deps.chat_event_sender.send(Arc::new(broadcast)));
    }
}

impl ProfileService for ProfileServer {
    impl_unary_handlers! {
        #[rate(8, 5)]
        get_profile, GetProfileRequest, GetProfileResponse;
        #[rate(4, 2)]
        get_app_data, GetAppDataRequest, GetAppDataResponse;
        #[rate(2, 5)]
        set_app_data, SetAppDataRequest, SetAppDataResponse;
        #[rate(4, 5)]
        update_profile, UpdateProfileRequest, UpdateProfileResponse;
    }
}

#[derive(Clone)]
pub struct ProfileTree {
    pub inner: Tree,
}

impl ProfileTree {
    impl_db_methods!(inner);

    pub async fn new(db: &Db) -> DbResult<Self> {
        let inner = db.open_tree(b"profile").await?;
        Ok(Self { inner })
    }

    pub async fn update_profile_logic(
        &self,
        user_id: u64,
        new_user_name: Option<String>,
        new_user_avatar: Option<String>,
        new_user_status: Option<i32>,
        new_is_bot: Option<bool>,
    ) -> ServerResult<()> {
        let key = make_user_profile_key(user_id);

        let mut profile = self
            .get(key)
            .await?
            .map_or_else(Profile::default, db::deser_profile);

        if let Some(new_username) = new_user_name {
            profile.user_name = new_username;
        }
        if let Some(new_avatar) = new_user_avatar {
            if new_avatar.is_empty() {
                profile.user_avatar = None;
            } else {
                profile.user_avatar = Some(new_avatar);
            }
        }
        if let Some(new_status) = new_user_status {
            profile.user_status = new_status;
        }
        if let Some(new_is_bot) = new_is_bot {
            profile.is_bot = new_is_bot;
        }

        let buf = rkyv_ser(&profile);
        self.insert(key, buf).await?;

        Ok(())
    }

    pub async fn get_profile_logic(&self, user_id: u64) -> ServerResult<Profile> {
        let key = make_user_profile_key(user_id);

        let profile = if let Some(profile_raw) = self.get(key).await? {
            db::deser_profile(profile_raw)
        } else {
            return Err(ServerError::NoSuchUser(user_id).into());
        };

        Ok(profile)
    }

    pub async fn does_username_exist(&self, username: &str) -> ServerResult<bool> {
        for res in self.scan_prefix(USER_PREFIX).await {
            let (_, value) = res?;
            let profile = db::rkyv_arch::<Profile>(&value);
            if profile.user_name == username {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub async fn does_user_exist(&self, user_id: u64) -> ServerResult<()> {
        self.contains_key(&make_user_profile_key(user_id))
            .await?
            .then(|| Ok(()))
            .unwrap_or_else(|| Err(ServerError::NoSuchUser(user_id).into()))
    }

    /// Converts a local user ID to the corresponding foreign user ID and the host
    pub async fn local_to_foreign_id(&self, local_id: u64) -> ServerResult<Option<(u64, SmolStr)>> {
        let key = make_local_to_foreign_user_key(local_id);

        Ok(self.get(key).await?.map(|raw| {
            let (raw_id, raw_host) = raw.split_at(size_of::<u64>());
            // Safety: safe since we split at u64 boundary.
            let foreign_id = deser_id(raw_id);
            // Safety: all stored hosts are valid UTF-8
            let host = (unsafe { std::str::from_utf8_unchecked(raw_host) }).into();
            (foreign_id, host)
        }))
    }

    /// Convert a foreign user ID to a local user ID
    pub async fn foreign_to_local_id(
        &self,
        foreign_id: u64,
        host: &str,
    ) -> ServerResult<Option<u64>> {
        let key = make_foreign_to_local_user_key(foreign_id, host);

        // Safety: we store u64's only for these keys
        Ok(self.get(key).await?.map(deser_id))
    }
}
