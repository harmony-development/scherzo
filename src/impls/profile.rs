use std::{convert::TryInto, mem::size_of};

use crate::{
    db::{self, profile::*, rkyv_ser, ArcTree, Db, DbResult},
    impls::chat::{EventContext, EventSub},
    ServerError,
};
use harmony_rust_sdk::api::{
    chat::Event,
    exports::hrpc::{server::ServerError as HrpcServerError, Request},
    profile::{profile_service_server::ProfileService, *},
};
use scherzo_derive::*;
use smol_str::SmolStr;
use triomphe::Arc;

use super::{
    auth::SessionMap,
    chat::{ChatTree, EventBroadcast, EventSender, PermCheck},
    Dependencies,
};

pub struct ProfileServer {
    profile_tree: ProfileTree,
    chat_tree: ChatTree,
    valid_sessions: SessionMap,
    pub broadcast_send: EventSender,
    disable_ratelimits: bool,
}

impl ProfileServer {
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            profile_tree: deps.profile_tree.clone(),
            chat_tree: deps.chat_tree.clone(),
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
        let broadcast = EventBroadcast::new(sub, Event::Profile(event), perm_check, context);

        drop(self.broadcast_send.send(Arc::new(broadcast)));
    }
}

#[harmony_rust_sdk::api::exports::hrpc::async_trait]
impl ProfileService for ProfileServer {
    type Error = ServerError;

    #[rate(32, 10)]
    async fn get_profile(
        &self,
        request: Request<GetProfileRequest>,
    ) -> Result<GetProfileResponse, HrpcServerError<Self::Error>> {
        auth!();

        let GetProfileRequest { user_id } = request.into_parts().0.into_message().await??;

        self.profile_tree
            .get_profile_logic(user_id)
            .map(|p| GetProfileResponse { profile: Some(p) })
            .map_err(Into::into)
    }

    #[rate(4, 1)]
    async fn get_app_data(
        &self,
        request: Request<GetAppDataRequest>,
    ) -> Result<GetAppDataResponse, HrpcServerError<Self::Error>> {
        auth!();

        let GetAppDataRequest { app_id } = request.into_parts().0.into_message().await??;
        let app_data = profile_get!(make_user_metadata_key(user_id, &app_id)).unwrap_or_default();

        Ok(GetAppDataResponse { app_data })
    }

    #[rate(2, 5)]
    async fn set_app_data(
        &self,
        request: Request<SetAppDataRequest>,
    ) -> Result<SetAppDataResponse, HrpcServerError<Self::Error>> {
        auth!();

        let SetAppDataRequest { app_id, app_data } =
            request.into_parts().0.into_message().await??;
        profile_insert!(make_user_metadata_key(user_id, &app_id) / app_data);

        Ok(SetAppDataResponse {})
    }

    #[rate(4, 5)]
    async fn update_profile(
        &self,
        request: Request<UpdateProfileRequest>,
    ) -> Result<UpdateProfileResponse, HrpcServerError<Self::Error>> {
        auth!();

        let UpdateProfileRequest {
            new_user_name,
            new_user_avatar,
            new_user_status,
            new_is_bot,
        } = request.into_parts().0.into_message().await??;

        self.profile_tree.update_profile_logic(
            user_id,
            new_user_name.clone(),
            new_user_avatar.clone(),
            new_user_status,
            new_is_bot,
        );

        self.send_event_through_chan(
            EventSub::Homeserver,
            stream_event::Event::ProfileUpdated(ProfileUpdated {
                user_id,
                new_username: new_user_name,
                new_avatar: new_user_avatar,
                new_status: new_user_status,
                new_is_bot,
            }),
            None,
            EventContext::new(self.chat_tree.calculate_users_seeing_user(user_id)),
        );

        Ok(UpdateProfileResponse {})
    }
}

#[derive(Clone)]
pub struct ProfileTree {
    pub inner: ArcTree,
}

impl ProfileTree {
    pub fn new(db: &dyn Db) -> DbResult<Self> {
        let inner = db.open_tree(b"profile")?;
        Ok(Self { inner })
    }

    pub fn update_profile_logic(
        &self,
        user_id: u64,
        new_user_name: Option<String>,
        new_user_avatar: Option<String>,
        new_user_status: Option<i32>,
        new_is_bot: Option<bool>,
    ) {
        let key = make_user_profile_key(user_id);

        let mut profile = pprofile_get!(key).map_or_else(Profile::default, db::deser_profile);

        if let Some(new_username) = new_user_name {
            profile.user_name = new_username;
        }
        if let Some(new_avatar) = new_user_avatar {
            profile.user_avatar = new_avatar;
        }
        if let Some(new_status) = new_user_status {
            profile.user_status = new_status;
        }
        if let Some(new_is_bot) = new_is_bot {
            profile.is_bot = new_is_bot;
        }

        let buf = rkyv_ser(&profile);
        pprofile_insert!(key / buf);
    }

    pub fn get_profile_logic(&self, user_id: u64) -> Result<Profile, ServerError> {
        let key = make_user_profile_key(user_id);

        let profile = if let Some(profile_raw) = pprofile_get!(key) {
            db::deser_profile(profile_raw)
        } else {
            return Err(ServerError::NoSuchUser(user_id));
        };

        Ok(profile)
    }

    pub fn does_user_exist(&self, user_id: u64) -> Result<(), ServerError> {
        self.inner
            .contains_key(&make_user_profile_key(user_id))
            .unwrap()
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::NoSuchUser(user_id)))
    }

    /// Converts a local user ID to the corresponding foreign user ID and the host
    pub fn local_to_foreign_id(&self, local_id: u64) -> Option<(u64, SmolStr)> {
        let key = make_local_to_foreign_user_key(local_id);

        pprofile_get!(key).map(|raw| {
            let (raw_id, raw_host) = raw.split_at(size_of::<u64>());
            // Safety: safe since we split at u64 boundary.
            let foreign_id = u64::from_be_bytes(unsafe { raw_id.try_into().unwrap_unchecked() });
            // Safety: all stored hosts are valid UTF-8
            let host = (unsafe { std::str::from_utf8_unchecked(raw_host) }).into();
            (foreign_id, host)
        })
    }

    /// Convert a foreign user ID to a local user ID
    pub fn foreign_to_local_id(&self, foreign_id: u64, host: &str) -> Option<u64> {
        let key = make_foreign_to_local_user_key(foreign_id, host);

        pprofile_get!(key)
            // Safety: we store u64's only for these keys
            .map(|raw| u64::from_be_bytes(unsafe { raw.try_into().unwrap_unchecked() }))
    }
}
