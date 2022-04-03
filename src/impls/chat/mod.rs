use std::{
    collections::HashSet, io::BufReader, iter, lazy::SyncOnceCell, mem::size_of, ops::Not,
    path::Path,
};

use crate::api::{
    chat::{
        attachment::ImageInfo, get_channel_messages_request::Direction, overrides::Reason,
        permission::has_permission, send_message_request::attachment::ImageInfo as SendImageInfo,
        stream_event, Message as HarmonyMessage, *,
    },
    exports::hrpc::{server::socket::Socket, Request},
    harmonytypes::{item_position, Empty, ItemPosition, Metadata},
    sync::{
        event::{
            Kind as DispatchKind, UserAddedToGuild as SyncUserAddedToGuild,
            UserRemovedFromGuild as SyncUserRemovedFromGuild,
        },
        Event as DispatchEvent,
    },
};
use image::{GenericImageView, ImageFormat};
use rand::{Rng, SeedableRng};
use scherzo_derive::*;
use smol_str::SmolStr;
use tokio::{
    sync::{broadcast::Sender as BroadcastSend, mpsc::UnboundedSender},
    task::JoinHandle,
};
use triomphe::Arc;

use crate::{
    db::{self, chat::*, rkyv_ser, Batch, Db, DbResult},
    impls::{get_time_secs, prelude::*, sync::EventDispatch},
};

use channels::*;
use guilds::*;
use invites::*;
use messages::*;
use moderation::*;
use permissions::*;

use super::media::FileHandle;

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
    PrivateChannel(u64),
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
    pub const fn new(guild_id: u64, channel_id: Option<u64>, check_for: &'a str) -> Self {
        Self {
            guild_id,
            channel_id,
            check_for,
            must_be_guild_owner: false,
        }
    }

    pub fn maybe_new(guild_id: Option<u64>, channel_id: u64, check_for: &'a str) -> Option<Self> {
        guild_id.map(|guild_id| Self::new(guild_id, Some(channel_id), check_for))
    }

    pub fn must_be_guild_owner(mut self) -> Self {
        self.must_be_guild_owner = true;
        self
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

pub type EventCanceller = BroadcastSend<u64>;
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
            disable_ratelimits: deps.config.policy.ratelimit.disable,
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
        socket: Socket<StreamEventsResponse, StreamEventsRequest>,
    ) -> JoinHandle<Result<(), HrpcError>> {
        let (mut sock_tx, mut sock_rx) = socket.split();

        let mut rx = self.deps.chat_event_sender.subscribe();
        let chat_tree = self.deps.chat_tree.clone();

        let fut = async move {
            let mut subs = HashSet::with_hasher(ahash::RandomState::new());

            // add initial subs
            // TODO: optimize local guild fetching
            let user_guilds = chat_tree.get_user_guilds(user_id).await?;
            let initial_subs = user_guilds
                .into_iter()
                .filter_map(|g| g.server_id.is_empty().then(|| EventSub::Guild(g.guild_id)));
            let initial_subs =
                initial_subs.chain([EventSub::Actions, EventSub::Homeserver].into_iter());
            subs.extend(initial_subs);

            // keep track of failed writes and reads to decide if closing the socket is worth it
            let mut failed_writes: u8 = 0;
            let mut failed_reads: u8 = 0;
            // keep track of unsubscribed event sub, if we are then don't add new guilds
            let mut manual_sub_handling = false;

            loop {
                tokio::select! {
                    res = sock_rx.receive_message() => {
                        let req = match res {
                            Ok(req) => {
                                failed_reads = 0;
                                req
                            },
                            Err(err) => {
                                tracing::error!(
                                    { failed_reads = %failed_reads },
                                    "failed to read from sub read socket: {}", err,
                                );

                                failed_reads += 1;
                                if failed_reads > 5 {
                                    return Err(err.into());
                                } else {
                                    continue;
                                }
                            }
                        };
                        if let Some(req) = req.request {
                            use stream_events_request::*;

                            tracing::debug!("got new stream events request");

                            let sub = match req {
                                Request::SubscribeToPrivateChannel(_) => todo!("private channels"),
                                Request::SubscribeToGuild(SubscribeToGuild { guild_id }) => {
                                    match chat_tree.check_guild_user(guild_id, user_id).await {
                                        Ok(_) => EventSub::Guild(guild_id),
                                        Err(err) => {
                                            tracing::error!("{}", err);
                                            continue;
                                        }
                                    }
                                }
                                Request::SubscribeToActions(SubscribeToActions {}) => EventSub::Actions,
                                Request::SubscribeToHomeserverEvents(SubscribeToHomeserverEvents {}) => {
                                    EventSub::Homeserver
                                }
                                Request::UnsubscribeFromAll(UnsubscribeFromAll {}) => {
                                    subs.clear();
                                    manual_sub_handling = true;
                                    continue;
                                }
                            };

                            subs.insert(sub);
                        }
                    }
                    Ok(broadcast) = rx.recv() => {
                        // handle automatic sub handling BEFORE all the other logic because otherwise
                        // `subs.contains()` will just return
                        if manual_sub_handling.not() {
                            match &broadcast.event {
                                Event::Chat(stream_event::Event::GuildAddedToList(guild)) => subs.insert(EventSub::Guild(guild.guild_id)),
                                Event::Chat(stream_event::Event::GuildRemovedFromList(guild)) => subs.remove(&EventSub::Guild(guild.guild_id)),
                                _ => false,
                            };
                        }

                        if !subs.contains(&broadcast.sub) {
                            continue;
                        }

                        if !broadcast.context.user_ids.is_empty() && !broadcast.context.user_ids.contains(&user_id) {
                            continue;
                        }

                        let perm_check = match broadcast.perm_check {
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
                        };


                        if !perm_check {
                            continue;
                        }

                        tracing::debug!("writing event to socket");

                        let write_res = sock_tx
                            .send_message(StreamEventsResponse {
                                event: Some(broadcast.event.clone().into()),
                            })
                            .await;

                        match write_res {
                            Ok(_) => failed_writes = 0,
                            Err(err) => {
                                tracing::error!(
                                    "couldnt write to stream events socket: {}",
                                    err
                                );
                                failed_writes += 1;
                                if failed_writes > 5 {
                                    return Err(err.into());
                                }
                            }
                        }
                    }
                    else => tokio::task::yield_now().await,
                }
            }

            #[allow(unreachable_code)]
            Ok(())
        };

        tokio::spawn(fut)
    }

    #[inline(always)]
    fn broadcast(
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
                self.broadcast(
                    EventSub::Homeserver,
                    stream_event::Event::GuildRemovedFromList(stream_event::GuildRemovedFromList {
                        guild_id,
                        server_id: String::new(),
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
                self.broadcast(
                    EventSub::Homeserver,
                    stream_event::Event::GuildAddedToList(stream_event::GuildAddedToList {
                        guild_id,
                        server_id: String::new(),
                    }),
                    None,
                    EventContext::new(vec![user_id]),
                );
            }
        }
        Ok(())
    }

    fn send_new_reaction_event(
        &self,
        guild_id: Option<u64>,
        channel_id: u64,
        message_id: u64,
        reaction: Reaction,
    ) {
        self.broadcast(
            guild_id.map_or(EventSub::PrivateChannel(channel_id), EventSub::Guild),
            stream_event::Event::NewReactionAdded(stream_event::NewReactionAdded {
                guild_id,
                channel_id,
                message_id,
                reaction: Some(reaction),
            }),
            PermCheck::maybe_new(guild_id, channel_id, all_permissions::MESSAGES_VIEW),
            EventContext::empty(),
        );
    }

    fn send_added_reaction_event(
        &self,
        guild_id: Option<u64>,
        channel_id: u64,
        message_id: u64,
        data: String,
    ) {
        self.broadcast(
            guild_id.map_or(EventSub::PrivateChannel(channel_id), EventSub::Guild),
            stream_event::Event::ReactionAdded(stream_event::ReactionAdded {
                guild_id,
                channel_id,
                message_id,
                reaction_data: data,
            }),
            PermCheck::maybe_new(guild_id, channel_id, all_permissions::MESSAGES_VIEW),
            EventContext::empty(),
        );
    }

    fn send_removed_reaction_event(
        &self,
        guild_id: Option<u64>,
        channel_id: u64,
        message_id: u64,
        data: String,
    ) {
        self.broadcast(
            guild_id.map_or(EventSub::PrivateChannel(channel_id), EventSub::Guild),
            stream_event::Event::ReactionRemoved(stream_event::ReactionRemoved {
                guild_id,
                channel_id,
                message_id,
                reaction_data: data,
            }),
            PermCheck::maybe_new(guild_id, channel_id, all_permissions::MESSAGES_VIEW),
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
        update_message_content, UpdateMessageContentRequest, UpdateMessageContentResponse;
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
        has_permission, HasPermissionRequest, HasPermissionResponse;
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
        #[rate(4, 5)]
        get_pinned_messages, GetPinnedMessagesRequest, GetPinnedMessagesResponse;
        #[rate(4, 10)]
        pin_message, PinMessageRequest, PinMessageResponse;
        #[rate(4, 10)]
        unpin_message, UnpinMessageRequest, UnpinMessageResponse;
        #[rate(5, 7)]
        add_reaction, AddReactionRequest, AddReactionResponse;
        #[rate(5, 7)]
        remove_reaction, RemoveReactionRequest, RemoveReactionResponse;
        #[rate(2, 60)]
        grant_ownership, GrantOwnershipRequest, GrantOwnershipResponse;
        #[rate(2, 60)]
        give_up_ownership, GiveUpOwnershipRequest, GiveUpOwnershipResponse;
        create_private_channel, CreatePrivateChannelRequest, CreatePrivateChannelResponse;
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
pub struct AdminGuildKeys {
    pub guild_id: u64,
    pub cmd_id: u64,
}

impl AdminGuildKeys {
    pub async fn new(chat_tree: &ChatTree) -> ServerResult<Option<AdminGuildKeys>> {
        Ok(chat_tree.get(ADMIN_GUILD_KEY).await?.map(|raw| {
            let (gid_raw, cmd_raw) = raw.split_at(size_of::<u64>());
            let guild_id = deser_id(gid_raw);
            let cmd_id = deser_id(cmd_raw);

            AdminGuildKeys { guild_id, cmd_id }
        }))
    }

    pub fn check_if_cmd(&self, guild_id: u64, channel_id: u64) -> bool {
        (self.guild_id, self.cmd_id) == (guild_id, channel_id)
    }
}

#[derive(Clone)]
pub struct ChatTree {
    pub chat_tree: Tree,
    pub admin_guild_keys: SyncOnceCell<AdminGuildKeys>,
}

impl ChatTree {
    impl_db_methods!(chat_tree);

    pub async fn new(db: &Db) -> DbResult<Self> {
        let chat_tree = db.open_tree(b"chat").await?;
        Ok(Self {
            chat_tree,
            admin_guild_keys: SyncOnceCell::new(),
        })
    }

    pub async fn check_channel_user(
        &self,
        guild_id: Option<u64>,
        channel_id: u64,
        user_id: u64,
    ) -> ServerResult<()> {
        if let Some(guild_id) = guild_id {
            self.check_guild_user_channel(guild_id, user_id, channel_id)
                .await
        } else {
            self.check_private_channel_user(channel_id, user_id).await
        }
    }

    pub async fn is_user_in_private_channel(
        &self,
        channel_id: u64,
        user_id: u64,
    ) -> ServerResult<bool> {
        self.contains_key(&make_member_key_pc(channel_id, user_id))
            .await
            .map_err(Into::into)
    }

    pub async fn check_user_in_private_channel(
        &self,
        channel_id: u64,
        user_id: u64,
    ) -> ServerResult<()> {
        self.is_user_in_private_channel(channel_id, user_id)
            .await?
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::UserNotInPrivateChannel {
                channel_id,
                user_id,
            }))
            .map_err(Into::into)
    }

    pub async fn is_user_in_guild(&self, guild_id: u64, user_id: u64) -> ServerResult<bool> {
        self.contains_key(&make_member_key(guild_id, user_id))
            .await
            .map_err(Into::into)
    }

    pub async fn check_user_in_guild(&self, guild_id: u64, user_id: u64) -> ServerResult<()> {
        self.is_user_in_guild(guild_id, user_id)
            .await?
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::UserNotInGuild { guild_id, user_id }))
            .map_err(Into::into)
    }

    pub async fn does_private_channel_exist(&self, channel_id: u64) -> ServerResult<bool> {
        self.contains_key(&make_pc_key(channel_id))
            .await
            .map_err(Into::into)
    }

    pub async fn check_private_channel_exist(&self, channel_id: u64) -> ServerResult<()> {
        self.does_private_channel_exist(channel_id)
            .await?
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::NoSuchPrivateChannel(channel_id)))
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
        self.check_user_in_guild(guild_id, user_id).await
    }

    #[inline(always)]
    pub async fn check_guild(&self, guild_id: u64) -> ServerResult<()> {
        self.does_guild_exist(guild_id).await
    }

    pub async fn check_private_channel_user(
        &self,
        channel_id: u64,
        user_id: u64,
    ) -> ServerResult<()> {
        self.check_private_channel_exist(channel_id).await?;
        self.check_user_in_private_channel(channel_id, user_id)
            .await?;
        Ok(())
    }

    pub async fn get_message_logic(
        &self,
        guild_id: Option<u64>,
        channel_id: u64,
        message_id: u64,
    ) -> ServerResult<(HarmonyMessage, Vec<u8>)> {
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

    pub async fn get_user_guilds(&self, user_id: u64) -> ServerResult<Vec<GuildListEntry>> {
        let prefix = make_guild_list_key_prefix(user_id);
        let guild_list = self
            .scan_prefix(&prefix)
            .await
            .try_fold(Vec::new(), |mut all, res| {
                let (guild_id_raw, _) = res?;
                let (id_raw, host_raw) = guild_id_raw
                    .split_at(prefix.len())
                    .1
                    .split_at(size_of::<u64>());

                // Safety: this unwrap can never cause UB since we split at u64 boundary
                let guild_id = u64::from_be_bytes(unsafe { id_raw.try_into().unwrap_unchecked() });
                // Safety: we never store non UTF-8 hosts, so this can't cause UB
                let host = unsafe { std::str::from_utf8_unchecked(host_raw) };

                all.push(GuildListEntry {
                    guild_id,
                    server_id: host.to_string(),
                });

                ServerResult::Ok(all)
            })?;
        Ok(guild_list)
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
                    let inv_guild_id = deser_id(inv_guild_id_raw);
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
                all.push(deser_id(id.split_at(prefix.len()).1));
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
                // Safety: this unwrap is safe since we check if it's a valid u64 beforehand
                let channel_id = deser_id(key.split_at(prefix.len()).1);

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
        self.update_order_logic(role_id, position, &make_guild_role_ordering_key(guild_id))
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
            &make_guild_chan_ordering_key(guild_id),
        )
        .await
    }

    #[inline(always)]
    pub async fn update_order_logic(
        &self,
        id: u64,
        position: Option<ItemPosition>,
        key: &[u8],
    ) -> Result<(), ServerError> {
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
        guild_id: Option<u64>,
        channel_id: u64,
        message_id: Option<u64>,
        direction: Option<Direction>,
        count: Option<u32>,
    ) -> ServerResult<GetChannelMessagesResponse> {
        let direction = direction.unwrap_or_default();

        // this is misleading... its actually the *next* message id to be used.
        // it doesnt matter here though.
        let last_message_id = self.get_last_message_id(guild_id, channel_id).await?;

        // if 1 it means that no messages have been sent yet
        if last_message_id == 1 {
            return Ok(GetChannelMessagesResponse {
                messages: Vec::new(),
                reached_bottom: true,
                reached_top: true,
            });
        }

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
                let prefix_len = guild_id.map_or_else(
                    || make_msg_prefix_pc(channel_id).len(),
                    |guild_id| make_msg_prefix(guild_id, channel_id).len(),
                );
                let message_id = deser_id(key.split_at(prefix_len).1);
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
                // Safety: this is safe since we split at u64 boundary
                raw.chunks_exact(size_of::<u64>()).map(deser_id).collect()
            }))
    }

    pub async fn has_permission_logic(
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

    // TODO: make this return bool so we can differ db errors
    // from actual permission error
    pub async fn check_perms(
        &self,
        guild_id: impl Into<Option<u64>>,
        channel_id: Option<u64>,
        user_id: u64,
        check_for: &str,
        must_be_guild_owner: bool,
    ) -> Result<(), ServerError> {
        let guild_id = guild_id.into();
        let Some(guild_id) = guild_id else {
            return Ok(());
        };
        let is_owner = self.is_user_guild_owner(guild_id, user_id).await?;
        if must_be_guild_owner {
            if is_owner {
                return Ok(());
            }
        } else if is_owner
            || self
                .has_permission_logic(guild_id, channel_id, user_id, check_for)
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
        if let Some(chan_id) = position.as_ref().map(|pos| pos.item_id) {
            self.does_channel_exist(guild_id, chan_id).await?;
        }

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
            make_next_msg_id_key_guild(guild_id, channel_id),
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
            let guild_id = deser_id(id_raw);
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
                    let role_id = deser_id(key.split_at(prefix.len()).1);
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

    pub async fn has_permission_request(
        &self,
        user_id: u64,
        request: HasPermissionRequest,
    ) -> ServerResult<HasPermissionResponse> {
        let HasPermissionRequest {
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

        let mut perms = Vec::with_capacity(check_for.len());
        for check_for in check_for {
            let ok = self
                .check_perms(guild_id, channel_id, check_as, &check_for, false)
                .await
                .is_ok();
            perms.push(Permission::new(check_for, ok));
        }

        Ok(HasPermissionResponse::new(perms))
    }

    pub async fn get_next_message_id(
        &self,
        guild_id: Option<u64>,
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
        guild_id: Option<u64>,
        channel_id: u64,
    ) -> Result<u64, ServerError> {
        let next_id_key = make_next_msg_id_key(guild_id, channel_id);
        let raw = self
            .get(&next_id_key)
            .await?
            .expect("no next message id for channel - this is a bug");
        // Safety: this won't cause UB since we only store u64
        let id = deser_id(raw);
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_message_logic(
        &self,
        guild_id: Option<u64>,
        channel_id: u64,
        user_id: u64,
        content: Content,
        overrides: Option<Overrides>,
        in_reply_to: Option<u64>,
        metadata: Option<Metadata>,
        actions: Vec<Action>,
    ) -> ServerResult<(u64, HarmonyMessage)> {
        let message_id = self.get_next_message_id(guild_id, channel_id).await?;
        let key = make_msg_key(guild_id, channel_id, message_id); // [tag:msg_key_u64]

        let created_at = get_time_millisecs();
        let edited_at = None;

        let message = HarmonyMessage {
            metadata,
            author_id: user_id,
            created_at,
            edited_at,
            content: Some(content),
            in_reply_to,
            overrides,
            actions,
            reactions: Vec::new(),
        };

        let value = db::rkyv_ser(&message);
        self.insert(key, value).await?;

        Ok((message_id, message))
    }

    pub fn process_message_overrides(&self, overrides: Option<&Overrides>) -> ServerResult<()> {
        if let Some(ov) = overrides {
            if ov.username.as_ref().map_or(false, String::is_empty) {
                bail!((
                    "h.override-username-cant-be-empty",
                    "message override username must not be empty if set"
                ));
            }
            if let Some(Reason::UserDefined(reason)) = ov.reason.as_ref() {
                if reason.is_empty() {
                    bail!((
                        "h.override-custom-reason-cant-be-empty",
                        "message override custom reason must not be empty if set"
                    ));
                }
            }
        }

        Ok(())
    }

    pub async fn process_image_info(
        &self,
        deps: &Dependencies,
        info: SendImageInfo,
        id: String,
        file_handle: FileHandle,
    ) -> Result<(String, ImageInfo), ServerError> {
        let SendImageInfo {
            caption,
            use_original,
        } = info;

        let media_root = &deps.config.media.media_root;
        let get_format = |path: &Path| {
            Result::<_, ServerError>::Ok(
                infer::get_from_path(path)?
                    .and_then(|t| ImageFormat::from_mime_type(t.mime_type()))
                    .expect("unsupported image format"),
            )
        };

        // TODO: improve this code
        let minithumbnail_id = format!("{}_jpegthumb", id);
        let minithumbnail_path = media_root.join(&minithumbnail_id);

        let id = use_original
            .not()
            .then(|| format!("{}_jpeg", id))
            .unwrap_or(id);

        let image_path = media_root.join(&id);
        let image_format =
            ImageFormat::from_mime_type(&file_handle.mime).expect("unsupported image format");

        let is_animated = image_format == ImageFormat::Gif;
        let is_webp = image_format == ImageFormat::WebP;

        let id_ = id.clone();
        let task_fn = move || -> Result<_, ServerError> {
            let _guard =
                tracing::debug_span!("image_processing", id = %id_, format = ?image_format)
                    .entered();
            if image_path.exists() && minithumbnail_path.exists() {
                use image::io::Reader as ImageReader;

                tracing::debug!("loading existing processed image and thumbnail");

                let image_format = get_format(&image_path)?;
                let ifile = BufReader::new(std::fs::File::open(&image_path)?);
                let mut ireader = ImageReader::new(ifile);
                ireader.set_format(image_format);
                let idimensions = ireader
                    .into_dimensions()
                    .map_err(ServerError::ImageProcessError)?;

                let minithumbnail_format = get_format(&minithumbnail_path)?;
                let mut minireader = ImageReader::open(&minithumbnail_path)?;
                minireader.set_format(minithumbnail_format);
                let minisize = minireader
                    .into_dimensions()
                    .map_err(ServerError::ImageProcessError)?;

                let minithumbnail = Minithumbnail {
                    width: minisize.0,
                    height: minisize.1,
                    data: std::fs::read(&minithumbnail_path)?,
                };

                Ok((idimensions, minithumbnail))
            } else {
                tracing::debug!("loading original image and processing");

                let raw = file_handle.read_blocking()?;

                let image = is_webp
                    .then(|| {
                        let decoder = webp::Decoder::new(&raw);
                        decoder.decode().map(|i| i.to_image())
                    })
                    .flatten();
                let image = match image {
                    Some(img) => img,
                    None => image::load_from_memory_with_format(&raw, image_format)
                        .map_err(ServerError::ImageProcessError)?,
                };

                let image_size = image.dimensions();
                let minithumbnail = image.thumbnail(64, 64);
                if use_original.not() {
                    let encoded = is_animated.not().then(|| {
                        let rgba = image.into_rgba8();
                        let encoder =
                            webp::Encoder::from_rgba(rgba.as_ref(), rgba.width(), rgba.height());
                        encoder.encode(100.0)
                    });
                    let image_raw = encoded.map(|m| m.to_vec()).unwrap_or(raw);
                    std::fs::write(&image_path, &image_raw)?;
                }

                let rgba = minithumbnail.into_rgba8();
                let encoder = webp::Encoder::from_rgba(rgba.as_ref(), rgba.width(), rgba.height());
                let encoded = encoder.encode(100.0).to_vec();

                std::fs::write(&minithumbnail_path, &encoded)?;

                Ok((
                    image_size,
                    Minithumbnail {
                        width: rgba.width(),
                        height: rgba.height(),
                        data: encoded,
                    },
                ))
            }
        };

        let ((width, height), minithumbnail) = tokio::task::spawn_blocking(task_fn)
            .await
            .expect("image process task panicked")?;

        let info = ImageInfo {
            width,
            height,
            minithumbnail: Some(minithumbnail),
            caption,
        };

        Ok((id, info))
    }

    pub async fn process_attachment_info(
        &self,
        deps: &Dependencies,
        info: Option<send_message_request::attachment::Info>,
        id: String,
        file_handle: FileHandle,
    ) -> Result<(String, Option<attachment::Info>), ServerError> {
        use send_message_request::attachment::Info;

        if let Some(info) = info {
            let (id, info) = match info {
                Info::Image(info) => {
                    let (id, info) = self.process_image_info(deps, info, id, file_handle).await?;

                    (id, attachment::Info::Image(info))
                }
            };

            Ok((id, Some(info)))
        } else if file_handle.mime != "image/svg+xml" && file_handle.mime.starts_with("image") {
            let (id, info) = self
                .process_image_info(deps, SendImageInfo::default(), id, file_handle)
                .await?;
            Ok((id, Some(attachment::Info::Image(info))))
        } else {
            Ok((id, None))
        }
    }

    pub async fn process_attachment(
        &self,
        deps: &Dependencies,
        send_message_request::Attachment { id, name, info }: send_message_request::Attachment,
    ) -> ServerResult<Attachment> {
        let file_handle = deps.media.get_file(&id).await?;

        let size = file_handle.size;
        let filename = name
            .is_empty()
            .then(|| file_handle.name.clone())
            .unwrap_or(name);
        let mimetype = file_handle.mime.clone();

        let (id, info) = self
            .process_attachment_info(deps, info, id, file_handle)
            .await?;

        Ok(Attachment {
            id,
            name: filename,
            mimetype,
            size: size as u32,
            info,
        })
    }

    pub async fn process_message_content(
        &self,
        deps: &Dependencies,
        content: Option<send_message_request::Content>,
    ) -> ServerResult<Content> {
        let Some(content) = content else {
            bail!(ServerError::MessageContentCantBeEmpty);
        };

        let is_text_empty = content.text.is_empty();
        let is_embeds_empty = content.embeds.is_empty();
        let is_attachments_empty = content.attachments.is_empty();

        // check if message content is empty
        if is_attachments_empty && is_embeds_empty && is_text_empty {
            bail!(ServerError::MessageContentCantBeEmpty);
        }

        let mut attachments = Vec::with_capacity(content.attachments.len());
        for attachment in content.attachments {
            attachments.push(self.process_attachment(deps, attachment).await?);
        }

        let content = Content {
            text: content.text,
            text_formats: content.text_formats,
            embeds: content.embeds,
            attachments,
            extra: None,
        };

        Ok(content)
    }

    pub async fn set_admin_guild_keys(&self, guild_id: u64, cmd_id: u64) -> ServerResult<()> {
        let value = [guild_id.to_be_bytes(), cmd_id.to_be_bytes()].concat();
        self.insert(ADMIN_GUILD_KEY, value).await?;
        Ok(())
    }

    pub async fn send_with_system(
        &self,
        guild_id: Option<u64>,
        channel_id: u64,
        content: Content,
    ) -> ServerResult<(u64, HarmonyMessage)> {
        let overrides = Overrides {
            username: Some("System".to_string()),
            reason: Some(overrides::Reason::SystemMessage(Empty {})),
            avatar: None,
        };
        self.send_message_logic(
            guild_id,
            channel_id,
            0,
            content,
            Some(overrides),
            None,
            None,
            Vec::new(),
        )
        .await
    }

    /// Removes a reaction by decrementing the count, if count is zero
    /// deletes the reaction from message reactions.
    ///
    /// Returns `true` if the message was deleted from message reactions.
    pub async fn remove_reaction(
        &self,
        user_id: u64,
        guild_id: Option<u64>,
        channel_id: u64,
        message_id: u64,
        data: &str,
    ) -> ServerResult<bool> {
        let react_key = guild_id.map_or_else(
            || make_user_reacted_msg_key_pc(channel_id, message_id, user_id, data),
            |guild_id| make_user_reacted_msg_key(guild_id, channel_id, message_id, user_id, data),
        );

        let reacted = self
            .chat_tree
            .contains_key(&react_key)
            .await
            .map_err(ServerError::from)?;
        if reacted.not() {
            bail!(("h.cant-remove-react", "cant remove react if didnt react"));
        }

        // TODO: validate the emote image_id is below a certain size
        let (mut message, message_key) = self
            .get_message_logic(guild_id, channel_id, message_id)
            .await?;

        let remove_index = message
            .reactions
            .iter_mut()
            .enumerate()
            .find(|(_, r)| r.data == data)
            .and_then(|(index, reaction)| {
                reaction.count = reaction.count.saturating_sub(1);
                (reaction.count == 0).then(|| index)
            });

        if let Some(index) = remove_index {
            message.reactions.remove(index);
        }

        self.insert(message_key, rkyv_ser(&message)).await?;
        self.remove(react_key).await?;

        Ok(remove_index.is_some())
    }

    /// Adds a new reaction by incrementing the count, if the reaction
    /// doesn't already exist adds it to the message reactions.
    ///
    /// Returns `true` if the reaction was new.
    #[allow(clippy::too_many_arguments)]
    pub async fn add_reaction(
        &self,
        user_id: u64,
        guild_id: Option<u64>,
        channel_id: u64,
        message_id: u64,
        data: String,
        name: String,
        kind: ReactionKind,
    ) -> ServerResult<bool> {
        let react_key = guild_id.map_or_else(
            || make_user_reacted_msg_key_pc(channel_id, message_id, user_id, &data),
            |guild_id| make_user_reacted_msg_key(guild_id, channel_id, message_id, user_id, &data),
        );

        let reacted = self
            .chat_tree
            .contains_key(&react_key)
            .await
            .map_err(ServerError::from)?;
        if reacted {
            bail!(("h.cant-react-multiple", "cant react multiple times"));
        }

        // TODO: validate the emote image_id is below a certain size
        let (mut message, message_key) = self
            .get_message_logic(guild_id, channel_id, message_id)
            .await?;

        let did_increment = message
            .reactions
            .iter_mut()
            .find(|r| r.data == data)
            .map(|reaction| {
                reaction.count = reaction.count.saturating_add(1);
            })
            .is_some();

        if did_increment.not() {
            let reaction = Reaction {
                count: 1,
                data,
                kind: kind.into(),
                name,
            };
            message.reactions.push(reaction);
        }

        self.insert(message_key, rkyv_ser(&message)).await?;
        self.insert(react_key, []).await?;

        Ok(did_increment.not())
    }

    pub async fn get_pinned_messages_logic(
        &self,
        guild_id: Option<u64>,
        channel_id: u64,
    ) -> Result<Vec<u64>, ServerError> {
        let pinned_msgs_raw = self.get(make_pinned_msgs_key(guild_id, channel_id)).await?;

        Ok(pinned_msgs_raw.map_or_else(Vec::new, |raw| {
            // SAFETY: chunks exact guarantees that the chunks we get are u64 long
            raw.chunks_exact(size_of::<u64>()).map(deser_id).collect()
        }))
    }

    pub async fn delete_invite_logic(&self, invite_id: String) -> Result<(), ServerError> {
        self.remove(make_invite_key(invite_id.as_str())).await?;
        Ok(())
    }
}
