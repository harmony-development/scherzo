use std::{
    collections::HashSet, convert::TryInto, future::Future, io::BufReader, mem::size_of, ops::Not,
    path::Path, str::FromStr,
};

use harmony_rust_sdk::api::{
    chat::{
        get_channel_messages_request::Direction, permission::has_permission, stream_event,
        FormattedText, Message as HarmonyMessage, *,
    },
    emote::Emote,
    exports::hrpc::{bail_result, server::socket::Socket, Request},
    harmonytypes::{item_position, Empty, ItemPosition, Metadata},
    rest::FileId,
    sync::{
        event::{
            Kind as DispatchKind, UserAddedToGuild as SyncUserAddedToGuild,
            UserRemovedFromGuild as SyncUserRemovedFromGuild,
        },
        Event as DispatchEvent,
    },
    Hmc,
};
use hrpc::server::socket::WriteSocket;
use image::GenericImageView;
use rand::{Rng, SeedableRng};
use scherzo_derive::*;
use smol_str::SmolStr;
use tokio::{
    sync::{
        broadcast::{error::TryRecvError, Sender as BroadcastSend},
        mpsc::{self, Receiver, UnboundedSender},
        oneshot,
    },
    task::JoinHandle,
};
use triomphe::Arc;

use crate::{
    db::{self, chat::*, rkyv_ser, Batch, Db, DbResult},
    impls::{
        get_time_secs,
        prelude::*,
        rest::download::{calculate_range, get_file_full, get_file_handle, is_id_jpeg, read_bufs},
        sync::EventDispatch,
    },
};

use channels::*;
use guilds::*;
use invites::*;
use messages::*;
use moderation::*;
use permissions::*;

pub mod channels;
pub mod guilds;
pub mod invites;
pub mod messages;
pub mod moderation;
pub mod permissions;
pub mod stream_events;
pub mod trigger_action;

pub const DEFAULT_ROLE_ID: u64 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventSub {
    Guild(u64),
    Homeserver,
    Actions,
}

#[derive(Debug, Clone, Copy)]
pub struct PermCheck<'a> {
    guild_id: u64,
    channel_id: Option<u64>,
    check_for: &'a str,
    must_be_guild_owner: bool,
}

impl<'a> PermCheck<'a> {
    pub const fn new(
        guild_id: u64,
        channel_id: Option<u64>,
        check_for: &'a str,
        must_be_guild_owner: bool,
    ) -> Self {
        Self {
            guild_id,
            channel_id,
            check_for,
            must_be_guild_owner,
        }
    }
}

#[derive(Debug)]
pub struct EventContext {
    user_ids: HashSet<u64, ahash::RandomState>,
}

impl EventContext {
    pub fn new(user_ids: Vec<u64>) -> Self {
        Self {
            user_ids: user_ids.into_iter().collect(),
        }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new())
    }
}

#[derive(Debug)]
pub struct EventBroadcast {
    sub: EventSub,
    event: Event,
    perm_check: Option<PermCheck<'static>>,
    context: EventContext,
}

impl EventBroadcast {
    pub fn new(
        sub: EventSub,
        event: Event,
        perm_check: Option<PermCheck<'static>>,
        context: EventContext,
    ) -> Self {
        Self {
            sub,
            event,
            perm_check,
            context,
        }
    }
}

pub type EventSender = BroadcastSend<Arc<EventBroadcast>>;
pub type EventDispatcher = UnboundedSender<EventDispatch>;

#[derive(Clone)]
pub struct ChatServer {
    deps: Arc<Dependencies>,
    disable_ratelimits: bool,
}

impl ChatServer {
    pub fn new(deps: Arc<Dependencies>) -> Self {
        Self {
            disable_ratelimits: deps.config.policy.disable_ratelimits,
            deps,
        }
    }

    pub fn batch(mut self) -> Self {
        self.disable_ratelimits = true;
        self
    }

    fn spawn_event_stream_processor(
        &self,
        user_id: u64,
        mut sub_rx: Receiver<EventSub>,
        mut tx: WriteSocket<StreamEventsResponse>,
        mut close_by_recv_rx: oneshot::Receiver<()>,
    ) -> JoinHandle<()> {
        async fn send_event(
            socket: &mut WriteSocket<StreamEventsResponse>,
            broadcast: &EventBroadcast,
            user_id: u64,
        ) -> bool {
            socket
                .send_message(StreamEventsResponse {
                    event: Some(broadcast.event.clone().into()),
                })
                .await
                .map_or_else(
                    |err| {
                        tracing::error!(
                            "couldnt write to stream events socket for user {}: {}",
                            user_id,
                            err
                        );
                        true
                    },
                    |_| false,
                )
        }

        let mut rx = self.deps.chat_event_sender.subscribe();
        let chat_tree = self.deps.chat_tree.clone();

        tokio::spawn(async move {
            let mut subs = HashSet::with_hasher(ahash::RandomState::new());

            loop {
                tokio::select! {
                    Some(sub) = sub_rx.recv() => {
                        subs.insert(sub);
                    }
                    Ok(broadcast) = async {
                        let mut result = rx.try_recv();
                        if let Err(TryRecvError::Lagged(behind)) = result {
                            tracing::error!("chat event receiver is lagging behind {} events...", behind);
                        }
                        while matches!(result, Err(TryRecvError::Lagged(_))) {
                            result = rx.try_recv();
                        }
                        match result {
                            Ok(event) => Ok(event),
                            Err(_) => rx.recv().await,
                        }
                    } => {
                        let check_perms = || async {
                            match broadcast.perm_check {
                                Some(PermCheck {
                                    guild_id,
                                    channel_id,
                                    check_for,
                                    must_be_guild_owner,
                                }) => {
                                    let perm = chat_tree.check_perms(
                                        guild_id,
                                        channel_id,
                                        user_id,
                                        check_for,
                                        must_be_guild_owner,
                                    ).await;

                                    matches!(perm, Ok(_) | Err(ServerError::EmptyPermissionQuery))
                                }
                                None => true,
                            }
                        };

                        if subs.contains(&broadcast.sub) {
                            if !broadcast.context.user_ids.is_empty() {
                                if broadcast.context.user_ids.contains(&user_id)
                                    && check_perms().await
                                    && send_event(&mut tx, broadcast.as_ref(), user_id).await
                                {
                                    return;
                                }
                            } else if check_perms().await
                                && send_event(&mut tx, broadcast.as_ref(), user_id).await
                            {
                                return;
                            }
                        }
                    }
                    _ = &mut close_by_recv_rx => return,
                    else => tokio::task::yield_now().await,
                }
            }
        })
    }

    #[inline(always)]
    fn send_event_through_chan(
        &self,
        sub: EventSub,
        event: stream_event::Event,
        perm_check: Option<PermCheck<'static>>,
        context: EventContext,
    ) {
        let broadcast = EventBroadcast {
            sub,
            event: Event::Chat(event),
            perm_check,
            context,
        };

        tracing::debug!(
            "broadcasting events to {} receivers",
            self.deps.chat_event_sender.receiver_count()
        );

        drop(self.deps.chat_event_sender.send(Arc::new(broadcast)));
    }

    #[inline(always)]
    fn dispatch_event(&self, target: SmolStr, event: DispatchKind) {
        let dispatch = EventDispatch {
            host: target,
            event: DispatchEvent { kind: Some(event) },
        };
        drop(self.deps.fed_event_dispatcher.send(dispatch));
    }

    async fn dispatch_guild_leave(&self, guild_id: u64, user_id: u64) -> ServerResult<()> {
        match self.deps.profile_tree.local_to_foreign_id(user_id).await? {
            Some((foreign_id, target)) => self.dispatch_event(
                target,
                DispatchKind::UserRemovedFromGuild(SyncUserRemovedFromGuild {
                    user_id: foreign_id,
                    guild_id,
                }),
            ),
            None => {
                self.deps
                    .chat_tree
                    .remove_guild_from_guild_list(user_id, guild_id, "")
                    .await?;
                self.send_event_through_chan(
                    EventSub::Homeserver,
                    stream_event::Event::GuildRemovedFromList(stream_event::GuildRemovedFromList {
                        guild_id,
                        homeserver: String::new(),
                    }),
                    None,
                    EventContext::new(vec![user_id]),
                );
            }
        }
        Ok(())
    }

    async fn dispatch_guild_join(&self, guild_id: u64, user_id: u64) -> ServerResult<()> {
        match self.deps.profile_tree.local_to_foreign_id(user_id).await? {
            Some((foreign_id, target)) => self.dispatch_event(
                target,
                DispatchKind::UserAddedToGuild(SyncUserAddedToGuild {
                    user_id: foreign_id,
                    guild_id,
                }),
            ),
            None => {
                self.deps
                    .chat_tree
                    .add_guild_to_guild_list(user_id, guild_id, "")
                    .await?;
                self.send_event_through_chan(
                    EventSub::Homeserver,
                    stream_event::Event::GuildAddedToList(stream_event::GuildAddedToList {
                        guild_id,
                        homeserver: String::new(),
                    }),
                    None,
                    EventContext::new(vec![user_id]),
                );
            }
        }
        Ok(())
    }

    fn send_reaction_event(
        &self,
        guild_id: u64,
        channel_id: u64,
        message_id: u64,
        reaction: Option<Reaction>,
    ) {
        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::ReactionUpdated(stream_event::ReactionUpdated {
                guild_id,
                channel_id,
                message_id,
                reaction,
            }),
            Some(PermCheck {
                guild_id,
                channel_id: Some(channel_id),
                check_for: all_permissions::MESSAGES_VIEW,
                must_be_guild_owner: false,
            }),
            EventContext::empty(),
        );
    }
}

impl chat_service_server::ChatService for ChatServer {
    impl_unary_handlers! {
        #[rate(1, 5)]
        create_guild, CreateGuildRequest, CreateGuildResponse;
        #[rate(3, 5)]
        create_invite, CreateInviteRequest, CreateInviteResponse;
        #[rate(4, 5)]
        create_channel, CreateChannelRequest, CreateChannelResponse;
        #[rate(5, 5)]
        get_guild_list, GetGuildListRequest, GetGuildListResponse;
        #[rate(5, 5)]
        get_guild, GetGuildRequest, GetGuildResponse;
        #[rate(5, 5)]
        get_guild_invites, GetGuildInvitesRequest, GetGuildInvitesResponse;
        #[rate(5, 5)]
        get_guild_members, GetGuildMembersRequest, GetGuildMembersResponse;
        #[rate(5, 5)]
        get_guild_channels, GetGuildChannelsRequest, GetGuildChannelsResponse;
        #[rate(10, 5)]
        get_channel_messages, GetChannelMessagesRequest, GetChannelMessagesResponse;
        #[rate(5, 5)]
        get_message, GetMessageRequest, GetMessageResponse;
        #[rate(2, 5)]
        update_guild_information, UpdateGuildInformationRequest, UpdateGuildInformationResponse;
        #[rate(2, 5)]
        update_channel_information, UpdateChannelInformationRequest, UpdateChannelInformationResponse;
        #[rate(10, 5)]
        update_channel_order, UpdateChannelOrderRequest, UpdateChannelOrderResponse;
        #[rate(1, 5)]
        update_all_channel_order, UpdateAllChannelOrderRequest, UpdateAllChannelOrderResponse;
        #[rate(5, 5)]
        update_message_text, UpdateMessageTextRequest, UpdateMessageTextResponse;
        #[rate(1, 15)]
        delete_guild, DeleteGuildRequest, DeleteGuildResponse;
        #[rate(4, 5)]
        delete_invite, DeleteInviteRequest, DeleteInviteResponse;
        #[rate(5, 5)]
        delete_channel, DeleteChannelRequest, DeleteChannelResponse;
        #[rate(7, 5)]
        delete_message, DeleteMessageRequest, DeleteMessageResponse;
        #[rate(3, 5)]
        join_guild, JoinGuildRequest, JoinGuildResponse;
        #[rate(3, 5)]
        leave_guild, LeaveGuildRequest, LeaveGuildResponse;
        #[rate(10, 5)]
        trigger_action, TriggerActionRequest, TriggerActionResponse;
        #[rate(15, 8)]
        send_message, SendMessageRequest, SendMessageResponse;
        #[rate(5, 7)]
        query_has_permission, QueryHasPermissionRequest, QueryHasPermissionResponse;
        #[rate(5, 7)]
        set_permissions, SetPermissionsRequest, SetPermissionsResponse;
        #[rate(7, 5)]
        get_permissions, GetPermissionsRequest, GetPermissionsResponse;
        #[rate(5, 7)]
        move_role, MoveRoleRequest, MoveRoleResponse;
        #[rate(7, 5)]
        get_guild_roles, GetGuildRolesRequest, GetGuildRolesResponse;
        #[rate(5, 7)]
        add_guild_role, AddGuildRoleRequest, AddGuildRoleResponse;
        #[rate(5, 7)]
        modify_guild_role, ModifyGuildRoleRequest, ModifyGuildRoleResponse;
        #[rate(5, 7)]
        delete_guild_role, DeleteGuildRoleRequest, DeleteGuildRoleResponse;
        #[rate(5, 7)]
        manage_user_roles, ManageUserRolesRequest, ManageUserRolesResponse;
        #[rate(10, 5)]
        get_user_roles, GetUserRolesRequest, GetUserRolesResponse;
        #[rate(4, 5)]
        typing, TypingRequest, TypingResponse;
        #[rate(2, 5)]
        preview_guild, PreviewGuildRequest, PreviewGuildResponse;
        #[rate(7, 5)]
        get_banned_users, GetBannedUsersRequest, GetBannedUsersResponse;
        #[rate(4, 5)]
        ban_user, BanUserRequest, BanUserResponse;
        #[rate(4, 5)]
        kick_user, KickUserRequest, KickUserResponse;
        #[rate(4, 5)]
        unban_user, UnbanUserRequest, UnbanUserResponse;
        get_pinned_messages, GetPinnedMessagesRequest, GetPinnedMessagesResponse;
        pin_message, PinMessageRequest, PinMessageResponse;
        unpin_message, UnpinMessageRequest, UnpinMessageResponse;
        #[rate(5, 7)]
        add_reaction, AddReactionRequest, AddReactionResponse;
        #[rate(5, 7)]
        remove_reaction, RemoveReactionRequest, RemoveReactionResponse;
        #[rate(2, 60)]
        grant_ownership, GrantOwnershipRequest, GrantOwnershipResponse;
        #[rate(2, 60)]
        give_up_ownership, GiveUpOwnershipRequest, GiveUpOwnershipResponse;
        create_room, CreateRoomRequest, CreateRoomResponse;
        create_direct_message, CreateDirectMessageRequest, CreateDirectMessageResponse;
        upgrade_room_to_guild, UpgradeRoomToGuildRequest, UpgradeRoomToGuildResponse;
        invite_user_to_guild, InviteUserToGuildRequest, InviteUserToGuildResponse;
        get_pending_invites, GetPendingInvitesRequest, GetPendingInvitesResponse;
        reject_pending_invite, RejectPendingInviteRequest, RejectPendingInviteResponse;
        ignore_pending_invite, IgnorePendingInviteRequest, IgnorePendingInviteResponse;
    }

    impl_ws_handlers! {
        #[rate(1, 10)]
        stream_events, StreamEventsRequest, StreamEventsResponse;
    }
}

#[derive(Clone)]
pub struct ChatTree {
    pub chat_tree: Tree,
}

impl ChatTree {
    impl_db_methods!(chat_tree);

    pub async fn new(db: &Db) -> DbResult<Self> {
        let chat_tree = db.open_tree(b"chat").await?;
        Ok(Self { chat_tree })
    }

    pub async fn is_user_in_guild(&self, guild_id: u64, user_id: u64) -> ServerResult<()> {
        self.contains_key(&make_member_key(guild_id, user_id))
            .await?
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::UserNotInGuild { guild_id, user_id }))
            .map_err(Into::into)
    }

    pub async fn does_guild_exist(&self, guild_id: u64) -> ServerResult<()> {
        self.contains_key(&guild_id.to_be_bytes())
            .await?
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::NoSuchGuild(guild_id)))
            .map_err(Into::into)
    }

    pub async fn does_channel_exist(
        &self,
        guild_id: u64,
        channel_id: u64,
    ) -> Result<(), ServerError> {
        self.contains_key(&make_chan_key(guild_id, channel_id))
            .await?
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::NoSuchChannel {
                guild_id,
                channel_id,
            }))
    }

    pub async fn does_role_exist(&self, guild_id: u64, role_id: u64) -> Result<(), ServerError> {
        self.contains_key(&make_guild_role_key(guild_id, role_id))
            .await?
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::NoSuchRole { guild_id, role_id }))
    }

    pub async fn is_user_banned_in_guild(&self, guild_id: u64, user_id: u64) -> ServerResult<bool> {
        self.contains_key(&make_banned_member_key(guild_id, user_id))
            .await
            .map_err(Into::into)
    }

    pub async fn get_guild_owners(&self, guild_id: u64) -> Result<Vec<u64>, ServerError> {
        self.get(guild_id.to_be_bytes().as_ref())
            .await?
            .map_or_else(
                || Err(ServerError::NoSuchGuild(guild_id)),
                |raw| Ok(db::deser_guild(raw).owner_ids),
            )
    }

    pub async fn is_user_guild_owner(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<bool, ServerError> {
        if user_id == 0 {
            return Ok(true);
        }

        let is_owner = self
            .get_guild_owners(guild_id)
            .await?
            .into_iter()
            .any(|owner| owner == user_id);

        Ok(is_owner)
    }

    pub async fn check_guild_user_channel(
        &self,
        guild_id: u64,
        user_id: u64,
        channel_id: u64,
    ) -> ServerResult<()> {
        self.check_guild_user(guild_id, user_id).await?;
        self.does_channel_exist(guild_id, channel_id).await?;
        Ok(())
    }

    pub async fn check_guild_user(&self, guild_id: u64, user_id: u64) -> ServerResult<()> {
        self.check_guild(guild_id).await?;
        self.is_user_in_guild(guild_id, user_id).await
    }

    #[inline(always)]
    pub async fn check_guild(&self, guild_id: u64) -> ServerResult<()> {
        self.does_guild_exist(guild_id).await
    }

    pub async fn get_message_logic(
        &self,
        guild_id: u64,
        channel_id: u64,
        message_id: u64,
    ) -> ServerResult<(HarmonyMessage, [u8; 26])> {
        let key = make_msg_key(guild_id, channel_id, message_id);

        let message = if let Some(msg) = self.get(&key).await? {
            db::deser_message(msg)
        } else {
            return Err((ServerError::NoSuchMessage {
                guild_id,
                channel_id,
                message_id,
            })
            .into());
        };

        Ok((message, key))
    }

    pub async fn get_guild_logic(&self, guild_id: u64) -> ServerResult<Guild> {
        let guild = if let Some(guild_raw) = self.get(guild_id.to_be_bytes().as_ref()).await? {
            db::deser_guild(guild_raw)
        } else {
            return Err(ServerError::NoSuchGuild(guild_id).into());
        };

        Ok(guild)
    }

    pub async fn put_guild_logic(&self, guild_id: u64, guild: Guild) -> ServerResult<()> {
        let buf = rkyv_ser(&guild);
        self.insert(guild_id.to_be_bytes(), buf)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    pub async fn get_guild_invites_logic(
        &self,
        guild_id: u64,
    ) -> ServerResult<GetGuildInvitesResponse> {
        let invites =
            self.scan_prefix(INVITE_PREFIX)
                .await
                .try_fold(Vec::new(), |mut all, res| {
                    let (key, value) = res?;
                    let (inv_guild_id_raw, invite_raw) = value.split_at(size_of::<u64>());
                    // Safety: this unwrap cannot fail since we split at u64 boundary
                    let inv_guild_id = u64::from_be_bytes(unsafe {
                        inv_guild_id_raw.try_into().unwrap_unchecked()
                    });
                    if guild_id == inv_guild_id {
                        let invite_id = unsafe {
                            std::str::from_utf8_unchecked(key.split_at(INVITE_PREFIX.len()).1)
                        };
                        let invite = db::deser_invite(invite_raw);
                        all.push(InviteWithId {
                            invite_id: invite_id.to_string(),
                            invite: Some(invite),
                        });
                    }
                    ServerResult::Ok(all)
                })?;

        Ok(GetGuildInvitesResponse { invites })
    }

    pub async fn get_guild_members_logic(
        &self,
        guild_id: u64,
    ) -> ServerResult<GetGuildMembersResponse> {
        let prefix = make_guild_mem_prefix(guild_id);
        let members = self
            .scan_prefix(&prefix)
            .await
            .try_fold(Vec::new(), |mut all, res| {
                let (id, _) = res?;
                // Safety: this unwrap cannot fail since after we split at prefix length, the remainder is a valid u64
                all.push(u64::from_be_bytes(unsafe {
                    id.split_at(prefix.len()).1.try_into().unwrap_unchecked()
                }));
                ServerResult::Ok(all)
            })?;

        Ok(GetGuildMembersResponse { members })
    }

    pub async fn get_guild_channels_logic(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<GetGuildChannelsResponse, ServerError> {
        let prefix = make_guild_chan_prefix(guild_id);
        let mut channels = Vec::new();
        for res in self.scan_prefix(&prefix).await {
            let (key, value) = res?;
            if key.len() == prefix.len() + size_of::<u64>() {
                let channel_id = u64::from_be_bytes(
                    // Safety: this unwrap is safe since we check if it's a valid u64 beforehand
                    unsafe { key.split_at(prefix.len()).1.try_into().unwrap_unchecked() },
                );

                let res_allowed = self
                    .check_perms(guild_id, Some(channel_id), user_id, "messages.view", false)
                    .await;
                let allowed = match res_allowed {
                    Ok(_) => true,
                    Err(ServerError::NotEnoughPermissions { .. }) => false,
                    Err(err) => return Err(err),
                };

                if allowed {
                    let channel = db::deser_chan(value);
                    channels.push(ChannelWithId {
                        channel_id,
                        channel: Some(channel),
                    });
                }
            }
        }

        if channels.is_empty() {
            return Ok(GetGuildChannelsResponse {
                channels: Vec::new(),
            });
        }

        let ordering_raw = self
            .chat_tree
            .get(&make_guild_chan_ordering_key(guild_id))
            .await
            .map_err(ServerError::from)?
            .unwrap_or_default();
        for (order_index, order_id) in db::make_u64_iter_logic(ordering_raw.as_ref()).enumerate() {
            if let Some(index) = channels.iter().position(|chan| chan.channel_id == order_id) {
                channels.swap(order_index, index);
            }
        }

        Ok(GetGuildChannelsResponse { channels })
    }

    #[inline(always)]
    pub async fn get_list_u64_logic(&self, key: &[u8]) -> Result<Vec<u64>, ServerError> {
        Ok(db::make_u64_iter_logic(
            self.chat_tree
                .get(key)
                .await
                .map_err(ServerError::from)?
                .unwrap_or_default()
                .as_ref(),
        )
        .collect())
    }

    #[inline(always)]
    pub fn serialize_list_u64_logic(&self, ordering: Vec<u64>) -> Vec<u8> {
        ordering
            .into_iter()
            .map(u64::to_be_bytes)
            .collect::<Vec<_>>()
            .concat()
    }

    pub async fn move_role_logic(
        &self,
        guild_id: u64,
        role_id: u64,
        position: Option<ItemPosition>,
    ) -> Result<(), ServerError> {
        self.update_order_logic(
            role_id,
            position,
            |role_id| self.does_role_exist(guild_id, role_id),
            &make_guild_role_ordering_key(guild_id),
        )
        .await
    }

    pub async fn update_channel_order_logic(
        &self,
        guild_id: u64,
        channel_id: u64,
        position: Option<ItemPosition>,
    ) -> Result<(), ServerError> {
        self.update_order_logic(
            channel_id,
            position,
            |channel_id| self.does_channel_exist(guild_id, channel_id),
            &make_guild_chan_ordering_key(guild_id),
        )
        .await
    }

    #[inline(always)]
    pub async fn update_order_logic<CheckFn, Fut>(
        &self,
        id: u64,
        position: Option<ItemPosition>,
        check_exists: CheckFn,
        key: &[u8],
    ) -> Result<(), ServerError>
    where
        Fut: Future<Output = Result<(), ServerError>>,
        CheckFn: Fn(u64) -> Fut,
    {
        let mut ordering = self.get_list_u64_logic(key).await?;

        let maybe_ord_index = |id: u64| ordering.iter().position(|oid| id.eq(oid));
        let maybe_replace_with = |ordering: &mut Vec<u64>, index| {
            ordering.insert(index, 0);
            if let Some(channel_index) = ordering.iter().position(|oid| id.eq(oid)) {
                ordering.remove(channel_index);
            }
            unsafe {
                *ordering
                    .iter_mut()
                    .find(|oid| 0.eq(*oid))
                    .unwrap_unchecked() = id;
            }
        };

        if let Some(position) = position {
            let item_id = position.item_id;
            check_exists(item_id).await?;
            match position.position() {
                item_position::Position::After => {
                    if let Some(index) = maybe_ord_index(item_id) {
                        maybe_replace_with(&mut ordering, index.saturating_add(1));
                    }
                }
                item_position::Position::BeforeUnspecified => {
                    if let Some(index) = maybe_ord_index(item_id) {
                        maybe_replace_with(&mut ordering, index);
                    }
                }
            }
        } else {
            ordering.push(id);
        }

        let serialized_ordering = self.serialize_list_u64_logic(ordering);
        self.insert(key, serialized_ordering).await?;

        Ok(())
    }

    pub async fn get_channel_messages_logic(
        &self,
        guild_id: u64,
        channel_id: u64,
        message_id: Option<u64>,
        direction: Option<Direction>,
        count: Option<u32>,
    ) -> ServerResult<GetChannelMessagesResponse> {
        let direction = direction.unwrap_or_default();

        let maybe_last_message_id = self
            .get_last_message_id(guild_id, channel_id)
            .await?
            .checked_sub(1);
        let last_message_id = match maybe_last_message_id {
            Some(id) => id,
            None => {
                return Ok(GetChannelMessagesResponse {
                    messages: Vec::new(),
                    reached_bottom: true,
                    reached_top: true,
                })
            }
        };

        let count = count.map_or_else(
            || {
                matches!(direction, Direction::Around)
                    .then(|| 12)
                    .unwrap_or(25)
            },
            |c| c as u64,
        );

        let to = message_id.unwrap_or(last_message_id);
        let to = (to > 1).then(|| to - 1).unwrap_or(1);
        let to_before = || (to > count).then(|| to - count).unwrap_or(1);
        let to_after = || (to + count).min(last_message_id);
        let (from, to) = match direction {
            Direction::BeforeUnspecified => (to_before(), to),
            Direction::After => (to, to_after()),
            Direction::Around => (to_before(), to_after()),
        };

        let from_key = make_msg_key(guild_id, channel_id, from);
        let to_key = make_msg_key(guild_id, channel_id, to);

        let messages = self
            .chat_tree
            .range((&from_key)..=(&to_key))
            .await
            .rev()
            .try_fold(Vec::new(), |mut all, res| {
                let (key, value) = res.map_err(ServerError::from)?;
                // Safety: this is safe since the only keys we get are message keys, which after stripping prefix are message IDs
                let message_id = u64::from_be_bytes(unsafe {
                    key.split_at(make_msg_prefix(guild_id, channel_id).len())
                        .1
                        .try_into()
                        .unwrap_unchecked()
                });
                let message = db::deser_message(value);
                all.push(MessageWithId {
                    message_id,
                    message: Some(message),
                });
                ServerResult::Ok(all)
            })?;

        Ok(GetChannelMessagesResponse {
            reached_top: from == 1,
            reached_bottom: to == last_message_id,
            messages,
        })
    }

    pub async fn get_user_roles_logic(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> ServerResult<Vec<u64>> {
        let key = make_guild_user_roles_key(guild_id, user_id);
        Ok(self
            .chat_tree
            .get(&key)
            .await
            .map_err(ServerError::from)?
            .map_or_else(Vec::default, |raw| {
                raw.chunks_exact(size_of::<u64>())
                    // Safety: this is safe since we split at u64 boundary
                    .map(|raw| u64::from_be_bytes(unsafe { raw.try_into().unwrap_unchecked() }))
                    .collect()
            }))
    }

    pub async fn query_has_permission_logic(
        &self,
        guild_id: u64,
        channel_id: Option<u64>,
        user_id: u64,
        check_for: &str,
    ) -> Result<bool, ServerError> {
        let key = make_guild_user_roles_key(guild_id, user_id);
        let user_roles = self.get_list_u64_logic(&key).await?;

        if let Some(channel_id) = channel_id {
            for role_id in &user_roles {
                let perms = self
                    .get_permissions_logic(guild_id, Some(channel_id), *role_id)
                    .await?;
                let is_allowed =
                    has_permission(perms.iter().map(|(m, ok)| (m.as_str(), *ok)), check_for);
                if let Some(true) = is_allowed {
                    return Ok(true);
                }
            }
        }

        for role_id in user_roles {
            let perms = self.get_permissions_logic(guild_id, None, role_id).await?;
            let is_allowed =
                has_permission(perms.iter().map(|(m, ok)| (m.as_str(), *ok)), check_for);
            if let Some(true) = is_allowed {
                return Ok(true);
            }
        }

        Ok(false)
    }

    pub async fn check_perms(
        &self,
        guild_id: u64,
        channel_id: Option<u64>,
        user_id: u64,
        check_for: &str,
        must_be_guild_owner: bool,
    ) -> Result<(), ServerError> {
        let is_owner = self.is_user_guild_owner(guild_id, user_id).await?;
        if must_be_guild_owner {
            if is_owner {
                return Ok(());
            }
        } else if is_owner
            || self
                .query_has_permission_logic(guild_id, channel_id, user_id, check_for)
                .await?
        {
            return Ok(());
        }
        Err(ServerError::NotEnoughPermissions {
            must_be_guild_owner,
            missing_permission: check_for.into(),
        })
    }

    pub async fn kick_user_logic(&self, guild_id: u64, user_id: u64) -> ServerResult<()> {
        let mut batch = Batch::default();
        batch.remove(make_member_key(guild_id, user_id));
        batch.remove(make_guild_user_roles_key(guild_id, user_id));
        self.chat_tree
            .apply_batch(batch)
            .await
            .map_err(ServerError::DbError)
            .map_err(Into::into)
    }

    pub async fn manage_user_roles_logic(
        &self,
        guild_id: u64,
        user_id: u64,
        give_role_ids: Vec<u64>,
        take_role_ids: Vec<u64>,
    ) -> ServerResult<Vec<u64>> {
        let mut roles = self.get_user_roles_logic(guild_id, user_id).await?;
        for role_id in give_role_ids {
            self.does_role_exist(guild_id, role_id).await?;
            roles.push(role_id);
        }
        for role_id in take_role_ids {
            self.does_role_exist(guild_id, role_id).await?;
            if let Some(index) = roles.iter().position(|oid| role_id.eq(oid)) {
                roles.remove(index);
            }
        }

        let key = make_guild_user_roles_key(guild_id, user_id);
        let ser_roles = self.serialize_list_u64_logic(roles.clone());
        self.insert(key, ser_roles).await?;

        Ok(roles)
    }

    pub async fn add_default_role_to(&self, guild_id: u64, user_id: u64) -> ServerResult<()> {
        self.manage_user_roles_logic(guild_id, user_id, vec![DEFAULT_ROLE_ID], Vec::new())
            .await
            .map(|_| ())
    }

    pub async fn add_guild_role_logic(
        &self,
        guild_id: u64,
        role_id: Option<u64>,
        role: Role,
    ) -> ServerResult<u64> {
        let (role_id, key) = if let Some(id) = role_id {
            (id, make_guild_role_key(guild_id, id))
        } else {
            let mut rng = rand::rngs::SmallRng::from_entropy();
            let mut role_id = rng.gen_range(1..u64::MAX);
            let mut key = make_guild_role_key(guild_id, role_id);
            while self.contains_key(&key).await? {
                role_id = rng.gen_range(1..u64::MAX);
                key = make_guild_role_key(guild_id, role_id);
            }
            (role_id, key)
        };
        let ser_role = rkyv_ser(&role);
        self.insert(key, ser_role).await?;
        self.move_role_logic(guild_id, role_id, None).await?;
        Ok(role_id)
    }

    pub async fn set_permissions_logic(
        &self,
        guild_id: u64,
        channel_id: Option<u64>,
        role_id: u64,
        perms_to_give: Vec<Permission>,
    ) -> ServerResult<()> {
        let mut batch = Batch::default();
        for perm in perms_to_give {
            let value = perm.ok.then(|| [1]).unwrap_or([0]);
            let key = channel_id.map_or_else(
                || make_guild_perm_key(guild_id, role_id, &perm.matches),
                |channel_id| make_channel_perm_key(guild_id, channel_id, role_id, &perm.matches),
            );
            batch.insert(key, value);
        }
        self.chat_tree
            .apply_batch(batch)
            .await
            .map_err(ServerError::from)?;
        Ok(())
    }

    pub async fn create_channel_logic(
        &self,
        guild_id: u64,
        channel_name: String,
        kind: ChannelKind,
        metadata: Option<Metadata>,
        position: Option<ItemPosition>,
    ) -> Result<u64, ServerError> {
        let (channel_id, key) = {
            let mut rng = rand::rngs::SmallRng::from_entropy();
            let mut channel_id = rng.gen_range(1..=u64::MAX);
            let mut key = make_chan_key(guild_id, channel_id);
            while self.contains_key(&key).await? {
                channel_id = rng.gen_range(1..=u64::MAX);
                key = make_chan_key(guild_id, channel_id);
            }
            (channel_id, key)
        };

        let channel = Channel {
            metadata,
            channel_name,
            kind: kind.into(),
        };
        let buf = rkyv_ser(&channel);

        let mut batch = Batch::default();
        batch.insert(key, buf);
        batch.insert(
            make_next_msg_id_key(guild_id, channel_id),
            1_u64.to_be_bytes(),
        );
        self.chat_tree.apply_batch(batch).await?;

        // Add from ordering list
        self.update_channel_order_logic(guild_id, channel_id, position)
            .await?;

        Ok(channel_id)
    }

    pub async fn create_guild_logic(
        &self,
        user_id: u64,
        name: String,
        picture: Option<String>,
        metadata: Option<Metadata>,
        kind: guild_kind::Kind,
    ) -> ServerResult<u64> {
        let guild_id = {
            let mut rng = rand::rngs::SmallRng::from_entropy();
            let mut guild_id = rng.gen_range(1..u64::MAX);
            while self.contains_key(&guild_id.to_be_bytes()).await? {
                guild_id = rng.gen_range(1..u64::MAX);
            }
            guild_id
        };

        let guild = Guild {
            name,
            picture,
            owner_ids: vec![user_id],
            metadata,
            kind: Some(GuildKind { kind: Some(kind) }),
        };
        let buf = rkyv_ser(&guild);

        self.insert(guild_id.to_be_bytes(), buf).await?;
        if user_id != 0 {
            self.insert(make_member_key(guild_id, user_id), []).await?;
        }

        // Some basic default setup
        let everyone_role_id = self
            .add_guild_role_logic(
                guild_id,
                // "everyone" role must have id 0 according to protocol
                Some(DEFAULT_ROLE_ID),
                Role {
                    name: "everyone".to_string(),
                    pingable: false,
                    ..Default::default()
                },
            )
            .await?;
        if user_id != 0 {
            self.add_default_role_to(guild_id, user_id).await?;
        }
        let def_perms = [
            "messages.send",
            "messages.view",
            "roles.get",
            "roles.user.get",
        ]
        .iter()
        .map(|m| Permission {
            matches: m.to_string(),
            ok: true,
        })
        .collect::<Vec<_>>();
        self.set_permissions_logic(guild_id, None, everyone_role_id, def_perms.clone())
            .await?;
        let channel_id = self
            .create_channel_logic(
                guild_id,
                "general".to_string(),
                ChannelKind::TextUnspecified,
                None,
                None,
            )
            .await?;

        self.set_permissions_logic(guild_id, Some(channel_id), everyone_role_id, def_perms)
            .await?;

        Ok(guild_id)
    }

    pub async fn create_invite_logic(
        &self,
        guild_id: u64,
        name: &str,
        possible_uses: u32,
    ) -> ServerResult<()> {
        let key = make_invite_key(name);

        if name.is_empty() {
            return Err(ServerError::InviteNameEmpty.into());
        }

        if self.get(&key).await?.is_some() {
            return Err(ServerError::InviteExists(name.to_string()).into());
        }

        let invite = Invite {
            possible_uses,
            use_count: 0,
        };
        let buf = rkyv_ser(&invite);

        self.insert(
            key,
            [guild_id.to_be_bytes().as_ref(), buf.as_ref()].concat(),
        )
        .await?;

        Ok(())
    }

    /// Calculates all users which can "see" the given user
    pub async fn calculate_users_seeing_user(&self, user_id: u64) -> ServerResult<Vec<u64>> {
        let prefix = make_guild_list_key_prefix(user_id);
        let mut all = Vec::new();
        for res in self.scan_prefix(&prefix).await {
            let (key, _) = res?;
            let (_, guild_id_raw) = key.split_at(prefix.len());
            let (id_raw, _) = guild_id_raw.split_at(size_of::<u64>());
            // Safety: safe since we split at u64 boundary
            let guild_id = u64::from_be_bytes(unsafe { id_raw.try_into().unwrap_unchecked() });
            let mut members = self.get_guild_members_logic(guild_id).await?.members;
            all.append(&mut members);
        }
        Ok(all)
    }

    /// Adds a guild to a user's guild list
    pub async fn add_guild_to_guild_list(
        &self,
        user_id: u64,
        guild_id: u64,
        homeserver: &str,
    ) -> ServerResult<()> {
        self.insert(
            [
                make_guild_list_key_prefix(user_id).as_ref(),
                guild_id.to_be_bytes().as_ref(),
                homeserver.as_bytes(),
            ]
            .concat(),
            [],
        )
        .await?;
        Ok(())
    }

    /// Removes a guild from a user's guild list
    pub async fn remove_guild_from_guild_list(
        &self,
        user_id: u64,
        guild_id: u64,
        homeserver: &str,
    ) -> ServerResult<()> {
        self.chat_tree
            .remove(&make_guild_list_key(user_id, guild_id, homeserver))
            .await
            .map(|_| ())
            .map_err(ServerError::from)
            .map_err(Into::into)
    }

    pub async fn get_guild_roles_logic(&self, guild_id: u64) -> ServerResult<Vec<RoleWithId>> {
        let prefix = make_guild_role_prefix(guild_id);
        self.chat_tree
            .scan_prefix(&prefix)
            .await
            .try_fold(Vec::new(), |mut all, res| {
                let (key, val) = res.map_err(ServerError::from)?;
                let maybe_role = (key.len() == make_guild_role_key(guild_id, 0).len()).then(|| {
                    let role = db::deser_role(val);
                    let role_id = u64::from_be_bytes(unsafe {
                        key.split_at(prefix.len()).1.try_into().unwrap_unchecked()
                    });
                    RoleWithId {
                        role_id,
                        role: Some(role),
                    }
                });
                if let Some(role) = maybe_role {
                    all.push(role);
                }
                Ok(all)
            })
    }

    pub async fn get_permissions_logic(
        &self,
        guild_id: u64,
        channel_id: Option<u64>,
        role_id: u64,
    ) -> Result<Vec<(SmolStr, bool)>, ServerError> {
        if let Some(channel_id) = channel_id {
            let prefix = make_role_channel_perms_prefix(guild_id, channel_id, role_id);

            self.chat_tree
                .scan_prefix(&prefix)
                .await
                .try_fold(Vec::new(), |mut all, res| {
                    let (key, value) = res.map_err(ServerError::from)?;
                    let matches_raw = key.split_at(prefix.len()).1;
                    let matches = unsafe { std::str::from_utf8_unchecked(matches_raw) };
                    let ok = value[0] != 0;
                    all.push((matches.into(), ok));
                    Ok(all)
                })
        } else {
            let prefix = make_role_guild_perms_prefix(guild_id, role_id);

            self.chat_tree
                .scan_prefix(&prefix)
                .await
                .try_fold(Vec::new(), |mut all, res| {
                    let (key, value) = res.map_err(ServerError::from)?;
                    let matches_raw = key.split_at(prefix.len()).1;
                    let matches = unsafe { std::str::from_utf8_unchecked(matches_raw) };
                    let ok = value[0] != 0;
                    all.push((matches.into(), ok));
                    Ok(all)
                })
        }
    }

    pub async fn query_has_permission_request(
        &self,
        user_id: u64,
        request: QueryHasPermissionRequest,
    ) -> ServerResult<QueryHasPermissionResponse> {
        let QueryHasPermissionRequest {
            guild_id,
            channel_id,
            check_for,
            r#as,
        } = request;

        self.check_guild_user(guild_id, user_id).await?;

        let check_as = r#as.unwrap_or(user_id);

        if r#as.is_some() {
            self.check_perms(guild_id, channel_id, user_id, "permissions.query", false)
                .await?;
        }

        if check_for.is_empty() {
            return Err(ServerError::EmptyPermissionQuery.into());
        }

        Ok(QueryHasPermissionResponse {
            ok: self
                .check_perms(guild_id, channel_id, check_as, &check_for, false)
                .await
                .is_ok(),
        })
    }

    pub async fn get_next_message_id(
        &self,
        guild_id: u64,
        channel_id: u64,
    ) -> Result<u64, ServerError> {
        let next_id_key = make_next_msg_id_key(guild_id, channel_id);
        let id = self.get_last_message_id(guild_id, channel_id).await?;
        self.chat_tree
            .insert(&next_id_key, &(id + 1).to_be_bytes())
            .await?;
        Ok(id)
    }

    pub async fn get_last_message_id(
        &self,
        guild_id: u64,
        channel_id: u64,
    ) -> Result<u64, ServerError> {
        let next_id_key = make_next_msg_id_key(guild_id, channel_id);
        let raw = self
            .chat_tree
            .get(&next_id_key)
            .await?
            .expect("no next message id for channel - this is a bug");
        // Safety: this won't cause UB since we only store u64
        let id = u64::from_be_bytes(unsafe { raw.try_into().unwrap_unchecked() });
        Ok(id)
    }

    pub async fn send_message_logic(
        &self,
        user_id: u64,
        request: SendMessageRequest,
    ) -> ServerResult<(u64, HarmonyMessage)> {
        let SendMessageRequest {
            guild_id,
            channel_id,
            content,
            echo_id: _,
            overrides,
            in_reply_to,
            metadata,
        } = request;

        let message_id = self.get_next_message_id(guild_id, channel_id).await?;
        let key = make_msg_key(guild_id, channel_id, message_id); // [tag:msg_key_u64]

        let created_at = get_time_secs();
        let edited_at = None;

        let message = HarmonyMessage {
            metadata,
            author_id: user_id,
            created_at,
            edited_at,
            content,
            in_reply_to,
            overrides,
            reactions: Vec::new(),
        };

        let value = db::rkyv_ser(&message);
        self.insert(key, value).await?;

        Ok((message_id, message))
    }

    pub async fn process_message_content(
        &self,
        content: Option<Content>,
        media_root: &Path,
        host: &str,
    ) -> Result<Content, ServerError> {
        use content::Content as MsgContent;

        let inner_content = content.and_then(|c| c.content);
        let content = if let Some(content) = inner_content {
            let content = match content {
                content::Content::TextMessage(text) => {
                    if text.content.as_ref().map_or(true, |f| f.text.is_empty()) {
                        return Err(ServerError::MessageContentCantBeEmpty);
                    }
                    content::Content::TextMessage(text)
                }
                content::Content::PhotoMessage(mut photos) => {
                    if photos.photos.is_empty() {
                        return Err(ServerError::MessageContentCantBeEmpty);
                    }
                    for photo in photos.photos.drain(..).collect::<Vec<_>>() {
                        // TODO: return error for invalid hmc
                        if let Ok(hmc) = Hmc::from_str(&photo.hmc) {
                            const FORMAT: image::ImageFormat = image::ImageFormat::Jpeg;

                            // TODO: check if the hmc host matches ours, if not fetch the image from the other host
                            let id = hmc.id();

                            let image_jpeg_id = format!("{}_{}", id, "jpeg");
                            let image_jpeg_path = media_root.join(&image_jpeg_id);
                            let minithumbnail_jpeg_id = format!("{}_{}", id, "jpegthumb");
                            let minithumbnail_jpeg_path = media_root.join(&minithumbnail_jpeg_id);

                            let ((file_size, isize), minithumbnail) = if image_jpeg_path.exists()
                                && minithumbnail_jpeg_path.exists()
                            {
                                let minifile = BufReader::new(
                                    tokio::fs::File::open(&minithumbnail_jpeg_path)
                                        .await
                                        .map_err(ServerError::from)?
                                        .into_std()
                                        .await,
                                );
                                let mut minireader = image::io::Reader::new(minifile);
                                minireader.set_format(FORMAT);
                                let minisize = minireader
                                    .into_dimensions()
                                    .map_err(|_| ServerError::InternalServerError)?;
                                let minithumbnail_jpeg = tokio::fs::read(&minithumbnail_jpeg_path)
                                    .await
                                    .map_err(ServerError::from)?;

                                let ifile = tokio::fs::File::open(&image_jpeg_path)
                                    .await
                                    .map_err(ServerError::from)?;
                                let file_size =
                                    ifile.metadata().await.map_err(ServerError::from)?.len();
                                let ifile = BufReader::new(ifile.into_std().await);
                                let mut ireader = image::io::Reader::new(ifile);
                                ireader.set_format(FORMAT);
                                let isize = ireader
                                    .into_dimensions()
                                    .map_err(|_| ServerError::InternalServerError)?;

                                (
                                    (file_size as u32, isize),
                                    Minithumbnail {
                                        width: minisize.0,
                                        height: minisize.1,
                                        data: minithumbnail_jpeg,
                                    },
                                )
                            } else {
                                let (_, _, data, _) = get_file_full(media_root, id).await?;

                                image::guess_format(&data).map_err(|_| ServerError::NotAnImage)?;

                                let image = image::load_from_memory(&data)
                                    .map_err(|_| ServerError::InternalServerError)?;
                                let image_size = image.dimensions();
                                let mut image_jpeg = Vec::new();
                                image
                                    .write_to(&mut image_jpeg, FORMAT)
                                    .map_err(|_| ServerError::InternalServerError)?;
                                let file_size = image_jpeg.len();
                                tokio::fs::write(&image_jpeg_path, image_jpeg)
                                    .await
                                    .map_err(ServerError::from)?;

                                let minithumbnail = image.thumbnail(64, 64);
                                let minithumb_size = minithumbnail.dimensions();
                                let mut minithumbnail_jpeg = Vec::new();
                                minithumbnail
                                    .write_to(&mut minithumbnail_jpeg, FORMAT)
                                    .map_err(|_| ServerError::InternalServerError)?;
                                tokio::fs::write(&minithumbnail_jpeg_path, &minithumbnail_jpeg)
                                    .await
                                    .map_err(ServerError::from)?;

                                (
                                    (file_size as u32, image_size),
                                    Minithumbnail {
                                        width: minithumb_size.0,
                                        height: minithumb_size.1,
                                        data: minithumbnail_jpeg,
                                    },
                                )
                            };

                            photos.photos.push(Photo {
                                hmc: Hmc::new(host, image_jpeg_id).unwrap().into(),
                                minithumbnail: Some(minithumbnail),
                                width: isize.0,
                                height: isize.1,
                                file_size,
                                ..photo
                            });
                        } else {
                            photos.photos.push(photo);
                        }
                    }
                    content::Content::PhotoMessage(photos)
                }
                content::Content::AttachmentMessage(mut files) => {
                    if files.files.is_empty() {
                        return Err(ServerError::MessageContentCantBeEmpty);
                    }
                    for attachment in files.files.drain(..).collect::<Vec<_>>() {
                        if let Ok(id) = FileId::from_str(&attachment.id) {
                            let fill_file_local = move |attachment: Attachment, id: String| async move {
                                let is_jpeg = is_id_jpeg(&id);
                                let (mut file, metadata) = get_file_handle(media_root, &id).await?;
                                let (filename_raw, mimetype_raw, _) =
                                    read_bufs(&mut file, is_jpeg).await?;
                                let (start, end) = calculate_range(
                                    &filename_raw,
                                    &mimetype_raw,
                                    &metadata,
                                    is_jpeg,
                                );
                                let size = end - start;

                                Result::<_, ServerError>::Ok(Attachment {
                                    name: attachment
                                        .name
                                        .is_empty()
                                        .then(|| unsafe {
                                            String::from_utf8_unchecked(filename_raw)
                                        })
                                        .unwrap_or(attachment.name),
                                    size: attachment
                                        .size
                                        .eq(&0)
                                        .then(|| size as u32)
                                        .unwrap_or(attachment.size),
                                    mimetype: attachment
                                        .mimetype
                                        .is_empty()
                                        .then(|| unsafe {
                                            String::from_utf8_unchecked(mimetype_raw)
                                        })
                                        .unwrap_or(attachment.mimetype),
                                    ..attachment
                                })
                            };
                            match id {
                                FileId::Hmc(hmc) => {
                                    // TODO: fetch file from remote host if its not local
                                    let id = hmc.id();
                                    files
                                        .files
                                        .push(fill_file_local(attachment, id.to_string()).await?);
                                }
                                FileId::Id(id) => {
                                    files.files.push(fill_file_local(attachment, id).await?)
                                }
                                _ => files.files.push(attachment),
                            }
                        } else {
                            files.files.push(attachment);
                        }
                    }
                    content::Content::AttachmentMessage(files)
                }
                content::Content::EmbedMessage(embed) => {
                    if embed.embeds.is_empty() {
                        return Err(ServerError::MessageContentCantBeEmpty);
                    }
                    content::Content::EmbedMessage(embed)
                }
                MsgContent::InviteAccepted(_)
                | MsgContent::InviteRejected(_)
                | MsgContent::RoomUpgradedToGuild(_) => {
                    return Err(ServerError::ContentCantBeSentByUser)
                }
            };
            Content {
                content: Some(content),
            }
        } else {
            return Err(ServerError::MessageContentCantBeEmpty);
        };

        Ok(content)
    }

    pub async fn get_admin_guild_keys(&self) -> ServerResult<Option<(u64, u64, u64)>> {
        Ok(self.get(ADMIN_GUILD_KEY).await?.map(|raw| {
            let (gid_raw, rest) = raw.split_at(size_of::<u64>());
            let guild_id = unsafe { u64::from_be_bytes(gid_raw.try_into().unwrap_unchecked()) };
            let (log_raw, cmd_raw) = rest.split_at(size_of::<u64>());
            let log_id = unsafe { u64::from_be_bytes(log_raw.try_into().unwrap_unchecked()) };
            let cmd_id = unsafe { u64::from_be_bytes(cmd_raw.try_into().unwrap_unchecked()) };
            (guild_id, log_id, cmd_id)
        }))
    }

    pub async fn set_admin_guild_keys(
        &self,
        guild_id: u64,
        log_id: u64,
        cmd_id: u64,
    ) -> ServerResult<()> {
        let value = [
            guild_id.to_be_bytes(),
            log_id.to_be_bytes(),
            cmd_id.to_be_bytes(),
        ]
        .concat();
        self.insert(ADMIN_GUILD_KEY, value).await?;
        Ok(())
    }

    pub async fn send_with_system(
        &self,
        guild_id: u64,
        channel_id: u64,
        content: content::Content,
    ) -> ServerResult<(u64, HarmonyMessage)> {
        let request = SendMessageRequest::default()
            .with_guild_id(guild_id)
            .with_channel_id(channel_id)
            .with_content(Content {
                content: Some(content),
            })
            .with_overrides(Overrides {
                username: Some("System".to_string()),
                reason: Some(overrides::Reason::SystemMessage(Empty {})),
                avatar: None,
            });
        self.send_message_logic(0, request).await
    }

    pub async fn update_reaction(
        &self,
        user_id: u64,
        guild_id: u64,
        channel_id: u64,
        message_id: u64,
        emote: Emote,
        add: bool,
    ) -> ServerResult<Option<Reaction>> {
        let react_key =
            make_user_reacted_msg_key(guild_id, channel_id, message_id, user_id, &emote.image_id);
        let reacted = self
            .chat_tree
            .contains_key(&react_key)
            .await
            .map_err(ServerError::from)?;
        if matches!((add, reacted), (true, true) | (false, false)) {
            return Ok(None);
        }

        // TODO: validate the emote image_id is below a certain size
        self.check_guild_user_channel(guild_id, user_id, channel_id)
            .await?;

        let (mut message, message_key) = self
            .get_message_logic(guild_id, channel_id, message_id)
            .await?;

        let mut batch = Batch::default();
        let reaction = if let Some(reaction) = message.reactions.iter_mut().find(|r| {
            r.emote
                .as_ref()
                .map_or(false, |e| e.image_id == emote.image_id)
        }) {
            reaction.count = add
                .then(|| reaction.count.saturating_add(1))
                .unwrap_or_else(|| reaction.count.saturating_sub(1));
            if reaction.count == 0 {
                batch.remove(react_key);
            }
            Some(reaction.clone())
        } else if add {
            let reaction = Reaction {
                count: 1,
                emote: Some(emote),
            };
            batch.insert(react_key, Vec::new());
            message.reactions.push(reaction.clone());
            Some(reaction)
        } else {
            None
        };

        batch.insert(message_key, rkyv_ser(&message));

        self.chat_tree
            .apply_batch(batch)
            .await
            .map_err(ServerError::from)?;

        Ok(reaction)
    }
}
