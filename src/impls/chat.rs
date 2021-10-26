use std::{
    collections::HashSet,
    convert::TryInto,
    io::BufReader,
    mem::size_of,
    ops::Not,
    path::{Path, PathBuf},
    str::FromStr,
};

use harmony_rust_sdk::api::{
    chat::{
        get_channel_messages_request::Direction, permission::has_permission, stream_event,
        FormattedText, Message as HarmonyMessage, *,
    },
    emote::Emote,
    exports::hrpc::{
        bail_result,
        body::BoxBody,
        server::{error::ServerError as HrpcServerError, socket::Socket},
        Request,
    },
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
use image::GenericImageView;
use rand::Rng;
use scherzo_derive::*;
use smol_str::SmolStr;
use tokio::{
    sync::{
        broadcast::Sender as BroadcastSend,
        mpsc::{self, Receiver, UnboundedSender},
    },
    task::JoinHandle,
};
use triomphe::Arc;

use crate::{
    db::{self, chat::*, rkyv_ser, Batch, Db, DbResult},
    impls::{
        auth, get_time_secs,
        rest::{calculate_range, get_file_full, get_file_handle, is_id_jpeg, read_bufs},
        sync::EventDispatch,
    },
    set_proto_name,
};

use super::{prelude::*, profile::ProfileTree, ActionProcesser};

pub const DEFAULT_ROLE_ID: u64 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventSub {
    Guild(u64),
    Homeserver,
    Actions,
}

#[derive(Clone, Copy)]
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
    host: String,
    media_root: PathBuf,
    valid_sessions: auth::SessionMap,
    chat_tree: ChatTree,
    profile_tree: ProfileTree,
    pub broadcast_send: EventSender,
    dispatch_tx: EventDispatcher,
    disable_ratelimits: bool,
    action_processor: ActionProcesser,
}

impl ChatServer {
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            host: deps.config.host.clone(),
            media_root: deps.config.media.media_root.clone(),
            valid_sessions: deps.valid_sessions.clone(),
            chat_tree: deps.chat_tree.clone(),
            profile_tree: deps.profile_tree.clone(),
            broadcast_send: deps.chat_event_sender.clone(),
            dispatch_tx: deps.fed_event_dispatcher.clone(),
            disable_ratelimits: deps.config.policy.disable_ratelimits,
            action_processor: deps.action_processor.clone(),
        }
    }

    fn spawn_event_stream_processor(
        &self,
        user_id: u64,
        mut sub_rx: Receiver<EventSub>,
        socket: Socket<StreamEventsRequest, StreamEventsResponse>,
    ) -> JoinHandle<()> {
        async fn send_event(
            socket: &Socket<StreamEventsRequest, StreamEventsResponse>,
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

        let mut rx = self.broadcast_send.subscribe();
        let chat_tree = self.chat_tree.clone();

        tokio::spawn(async move {
            let mut subs = HashSet::with_hasher(ahash::RandomState::new());

            loop {
                tokio::select! {
                    Some(sub) = sub_rx.recv() => {
                        subs.insert(sub);
                    }
                    Ok(broadcast) = rx.recv() => {
                        let check_perms = || {
                            broadcast.perm_check.map_or(true, |perm_check| {
                                let PermCheck {
                                    guild_id,
                                    channel_id,
                                    check_for,
                                    must_be_guild_owner,
                                } = perm_check;

                                let perm = chat_tree.check_perms(
                                    guild_id,
                                    channel_id,
                                    user_id,
                                    check_for,
                                    must_be_guild_owner,
                                );

                                // TODO: fix this trolley
                                matches!(perm, Ok(_) /*| Err(ServerError::EmptyPermissionQuery)*/)
                            })
                        };

                        if subs.contains(&broadcast.sub) {
                            if !broadcast.context.user_ids.is_empty() {
                                if broadcast.context.user_ids.contains(&user_id)
                                    && check_perms()
                                    && send_event(&socket, broadcast.as_ref(), user_id).await
                                {
                                    return;
                                }
                            } else if check_perms()
                                && send_event(&socket, broadcast.as_ref(), user_id).await
                            {
                                return;
                            }
                        }
                    }
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

        drop(self.broadcast_send.send(Arc::new(broadcast)));
    }

    #[inline(always)]
    fn dispatch_event(&self, target: SmolStr, event: DispatchKind) {
        let dispatch = EventDispatch {
            host: target,
            event: DispatchEvent { kind: Some(event) },
        };
        drop(self.dispatch_tx.send(dispatch));
    }

    fn dispatch_guild_leave(&self, guild_id: u64, user_id: u64) -> ServerResult<()> {
        match self.profile_tree.local_to_foreign_id(user_id)? {
            Some((foreign_id, target)) => self.dispatch_event(
                target,
                DispatchKind::UserRemovedFromGuild(SyncUserRemovedFromGuild {
                    user_id: foreign_id,
                    guild_id,
                }),
            ),
            None => {
                self.chat_tree
                    .remove_guild_from_guild_list(user_id, guild_id, "")?;
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

    fn dispatch_guild_join(&self, guild_id: u64, user_id: u64) -> ServerResult<()> {
        match self.profile_tree.local_to_foreign_id(user_id)? {
            Some((foreign_id, target)) => self.dispatch_event(
                target,
                DispatchKind::UserAddedToGuild(SyncUserAddedToGuild {
                    user_id: foreign_id,
                    guild_id,
                }),
            ),
            None => {
                self.chat_tree
                    .add_guild_to_guild_list(user_id, guild_id, "")?;
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

#[async_trait]
impl chat_service_server::ChatService for ChatServer {
    #[rate(1, 5)]
    async fn create_guild(
        &mut self,
        request: Request<CreateGuildRequest>,
    ) -> ServerResult<Response<CreateGuildResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let CreateGuildRequest {
            metadata,
            name,
            picture,
        } = request.into_message().await?;

        let guild_id = self
            .chat_tree
            .create_guild_logic(user_id, name, picture, metadata)?;

        self.dispatch_guild_join(guild_id, user_id)?;

        Ok((CreateGuildResponse { guild_id }).into_response())
    }

    #[rate(5, 5)]
    async fn create_invite(
        &mut self,
        request: Request<CreateInviteRequest>,
    ) -> ServerResult<Response<CreateInviteResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let CreateInviteRequest {
            guild_id,
            name,
            possible_uses,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "invites.manage.create", false)?;

        self.chat_tree
            .create_invite_logic(guild_id, name.as_str(), possible_uses)?;

        Ok((CreateInviteResponse { invite_id: name }).into_response())
    }

    #[rate(5, 5)]
    async fn create_channel(
        &mut self,
        request: Request<CreateChannelRequest>,
    ) -> ServerResult<Response<CreateChannelResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let CreateChannelRequest {
            guild_id,
            channel_name,
            kind,
            position,
            metadata,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "channels.manage.create", false)?;

        let channel_id = self.chat_tree.create_channel_logic(
            guild_id,
            channel_name.clone(),
            ChannelKind::from_i32(kind).unwrap_or_default(),
            metadata.clone(),
            position.clone(),
        )?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::CreatedChannel(stream_event::ChannelCreated {
                guild_id,
                channel_id,
                name: channel_name,
                position,
                kind,
                metadata,
            }),
            Some(PermCheck::new(
                guild_id,
                Some(channel_id),
                "messages.view",
                false,
            )),
            EventContext::empty(),
        );

        Ok((CreateChannelResponse { channel_id }).into_response())
    }

    #[rate(15, 5)]
    async fn get_guild_list(
        &mut self,
        request: Request<GetGuildListRequest>,
    ) -> ServerResult<Response<GetGuildListResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let prefix = make_guild_list_key_prefix(user_id);
        let guilds = self.chat_tree.chat_tree.scan_prefix(&prefix).try_fold(
            Vec::new(),
            |mut all, res| {
                let (guild_id_raw, _) = res.map_err(ServerError::from)?;
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
            },
        )?;

        Ok((GetGuildListResponse { guilds }).into_response())
    }

    #[rate(15, 5)]
    async fn get_guild(
        &mut self,
        request: Request<GetGuildRequest>,
    ) -> ServerResult<Response<GetGuildResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let GetGuildRequest { guild_id } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        self.chat_tree
            .get_guild_logic(guild_id)
            .map(|g| Response::new(GetGuildResponse { guild: Some(g) }))
    }

    #[rate(15, 5)]
    async fn get_guild_invites(
        &mut self,
        request: Request<GetGuildInvitesRequest>,
    ) -> ServerResult<Response<GetGuildInvitesResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let GetGuildInvitesRequest { guild_id } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "invites.view", false)?;

        self.chat_tree
            .get_guild_invites_logic(guild_id)
            .map(Response::new)
    }

    #[rate(15, 5)]
    async fn get_guild_members(
        &mut self,
        request: Request<GetGuildMembersRequest>,
    ) -> ServerResult<Response<GetGuildMembersResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let GetGuildMembersRequest { guild_id } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        self.chat_tree
            .get_guild_members_logic(guild_id)
            .map(Response::new)
    }

    #[rate(15, 5)]
    async fn get_guild_channels(
        &mut self,
        request: Request<GetGuildChannelsRequest>,
    ) -> ServerResult<Response<GetGuildChannelsResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let GetGuildChannelsRequest { guild_id } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        self.chat_tree
            .get_guild_channels_logic(guild_id, user_id)
            .map(Response::new)
    }

    #[rate(10, 5)]
    async fn get_channel_messages(
        &mut self,
        request: Request<GetChannelMessagesRequest>,
    ) -> ServerResult<Response<GetChannelMessagesResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let GetChannelMessagesRequest {
            guild_id,
            channel_id,
            message_id,
            direction,
            count,
        } = request.into_message().await?;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree
            .check_perms(guild_id, Some(channel_id), user_id, "messages.view", false)?;

        self.chat_tree
            .get_channel_messages_logic(
                guild_id,
                channel_id,
                message_id,
                direction.map(|val| Direction::from_i32(val).unwrap_or_default()),
                count,
            )
            .map(Response::new)
    }

    #[rate(10, 5)]
    async fn get_message(
        &mut self,
        request: Request<GetMessageRequest>,
    ) -> ServerResult<Response<GetMessageResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let request = request.into_message().await?;

        let GetMessageRequest {
            guild_id,
            channel_id,
            message_id,
        } = request;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree
            .check_perms(guild_id, Some(channel_id), user_id, "messages.view", false)?;

        let message = Some(
            self.chat_tree
                .get_message_logic(guild_id, channel_id, message_id)?
                .0,
        );

        Ok((GetMessageResponse { message }).into_response())
    }

    #[rate(2, 5)]
    async fn update_guild_information(
        &mut self,
        request: Request<UpdateGuildInformationRequest>,
    ) -> ServerResult<Response<UpdateGuildInformationResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let UpdateGuildInformationRequest {
            guild_id,
            new_name,
            new_picture,
            new_metadata,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        let mut guild_info = self.chat_tree.get_guild_logic(guild_id)?;

        self.chat_tree.check_perms(
            guild_id,
            None,
            user_id,
            "guild.manage.change-information",
            false,
        )?;

        if let Some(new_name) = new_name.clone() {
            guild_info.name = new_name;
        }
        if let Some(new_picture) = new_picture.clone() {
            guild_info.picture = Some(new_picture);
        }
        if let Some(new_metadata) = new_metadata.clone() {
            guild_info.metadata = Some(new_metadata);
        }

        self.chat_tree.put_guild_logic(guild_id, guild_info)?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::EditedGuild(stream_event::GuildUpdated {
                guild_id,
                new_name,
                new_picture,
                new_metadata,
            }),
            None,
            EventContext::empty(),
        );

        Ok((UpdateGuildInformationResponse {}).into_response())
    }

    #[rate(2, 5)]
    async fn update_channel_information(
        &mut self,
        request: Request<UpdateChannelInformationRequest>,
    ) -> ServerResult<Response<UpdateChannelInformationResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let UpdateChannelInformationRequest {
            guild_id,
            channel_id,
            new_name,
            new_metadata,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.check_perms(
            guild_id,
            Some(channel_id),
            user_id,
            "channels.manage.change-information",
            false,
        )?;

        let key = make_chan_key(guild_id, channel_id);
        let mut chan_info = if let Some(raw) = self.chat_tree.get(key)? {
            db::deser_chan(raw)
        } else {
            return Err(ServerError::NoSuchChannel {
                guild_id,
                channel_id,
            }
            .into());
        };

        if let Some(new_name) = new_name.clone() {
            chan_info.channel_name = new_name;
        }
        if let Some(new_metadata) = new_metadata.clone() {
            chan_info.metadata = Some(new_metadata);
        }

        let buf = rkyv_ser(&chan_info);
        self.chat_tree.insert(key, buf)?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::EditedChannel(stream_event::ChannelUpdated {
                guild_id,
                channel_id,
                new_name,
                new_metadata,
            }),
            Some(PermCheck::new(
                guild_id,
                Some(channel_id),
                "messages.view",
                false,
            )),
            EventContext::empty(),
        );

        Ok((UpdateChannelInformationResponse {}).into_response())
    }

    #[rate(10, 5)]
    async fn update_channel_order(
        &mut self,
        request: Request<UpdateChannelOrderRequest>,
    ) -> ServerResult<Response<UpdateChannelOrderResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let UpdateChannelOrderRequest {
            guild_id,
            channel_id,
            new_position,
        } = request.into_message().await?;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree.check_perms(
            guild_id,
            Some(channel_id),
            user_id,
            "channels.manage.move",
            false,
        )?;

        if let Some(position) = new_position {
            self.chat_tree.update_channel_order_logic(
                guild_id,
                channel_id,
                Some(position.clone()),
            )?;

            self.send_event_through_chan(
                EventSub::Guild(guild_id),
                stream_event::Event::EditedChannelPosition(stream_event::ChannelPositionUpdated {
                    guild_id,
                    channel_id,
                    new_position: Some(position),
                }),
                Some(PermCheck::new(
                    guild_id,
                    Some(channel_id),
                    "messages.view",
                    false,
                )),
                EventContext::empty(),
            );
        }

        Ok((UpdateChannelOrderResponse {}).into_response())
    }

    #[rate(1, 5)]
    async fn update_all_channel_order(
        &mut self,
        request: Request<UpdateAllChannelOrderRequest>,
    ) -> ServerResult<Response<UpdateAllChannelOrderResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let UpdateAllChannelOrderRequest {
            guild_id,
            channel_ids,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "channels.manage.move", false)?;

        for channel_id in &channel_ids {
            self.chat_tree.does_channel_exist(guild_id, *channel_id)?;
        }

        let prefix = make_guild_chan_prefix(guild_id);
        let channels = self.chat_tree.chat_tree.scan_prefix(&prefix).try_fold(
            Vec::new(),
            |mut all, res| {
                let (key, _) = res.map_err(ServerError::from)?;
                if key.len() == prefix.len() + size_of::<u64>() {
                    all.push(unsafe { u64::from_be_bytes(key.try_into().unwrap_unchecked()) });
                }
                ServerResult::Ok(all)
            },
        )?;

        for channel_id in channels {
            if !channel_ids.contains(&channel_id) {
                return Err(ServerError::UnderSpecifiedChannels.into());
            }
        }

        let key = make_guild_chan_ordering_key(guild_id);
        let serialized_ordering = self.chat_tree.serialize_list_u64_logic(channel_ids.clone());
        self.chat_tree.insert(key, serialized_ordering)?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::ChannelsReordered(stream_event::ChannelsReordered {
                guild_id,
                channel_ids,
            }),
            None,
            EventContext::empty(),
        );

        Ok((UpdateAllChannelOrderResponse {}).into_response())
    }

    #[rate(2, 5)]
    async fn update_message_text(
        &mut self,
        request: Request<UpdateMessageTextRequest>,
    ) -> ServerResult<Response<UpdateMessageTextResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let request = request.into_message().await?;

        let UpdateMessageTextRequest {
            guild_id,
            channel_id,
            message_id,
            new_content,
        } = request;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree
            .check_perms(guild_id, Some(channel_id), user_id, "messages.send", false)?;

        if new_content.as_ref().map_or(true, |f| f.text.is_empty()) {
            return Err(ServerError::MessageContentCantBeEmpty.into());
        }

        let (mut message, key) = self
            .chat_tree
            .get_message_logic(guild_id, channel_id, message_id)?;

        let msg_content = if let Some(content) = &mut message.content {
            content
        } else {
            message.content = Some(Content::default());
            message.content.as_mut().unwrap()
        };
        msg_content.content = Some(content::Content::TextMessage(content::TextContent {
            content: new_content.clone(),
        }));

        let edited_at = get_time_secs();
        message.edited_at = Some(edited_at);

        let buf = rkyv_ser(&message);
        self.chat_tree.insert(key, buf)?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::EditedMessage(Box::new(stream_event::MessageUpdated {
                guild_id,
                channel_id,
                message_id,
                edited_at,
                new_content,
            })),
            Some(PermCheck::new(
                guild_id,
                Some(channel_id),
                "messages.view",
                false,
            )),
            EventContext::empty(),
        );

        Ok((UpdateMessageTextResponse {}).into_response())
    }

    #[rate(1, 15)]
    async fn delete_guild(
        &mut self,
        request: Request<DeleteGuildRequest>,
    ) -> ServerResult<Response<DeleteGuildResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let DeleteGuildRequest { guild_id } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "guild.manage.delete", false)?;

        let guild_members = self.chat_tree.get_guild_members_logic(guild_id)?.members;

        let guild_data = self
            .chat_tree
            .chat_tree
            .scan_prefix(&guild_id.to_be_bytes())
            .try_fold(Vec::new(), |mut all, res| {
                all.push(res.map_err(ServerError::from)?.0);
                ServerResult::Ok(all)
            })?;

        let mut batch = Batch::default();
        for key in guild_data {
            batch.remove(key);
        }
        self.chat_tree
            .chat_tree
            .apply_batch(batch)
            .map_err(ServerError::DbError)?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::DeletedGuild(stream_event::GuildDeleted { guild_id }),
            None,
            EventContext::empty(),
        );

        let mut local_ids = Vec::new();
        for member_id in guild_members {
            match self.profile_tree.local_to_foreign_id(member_id)? {
                Some((foreign_id, target)) => self.dispatch_event(
                    target,
                    DispatchKind::UserRemovedFromGuild(SyncUserRemovedFromGuild {
                        user_id: foreign_id,
                        guild_id,
                    }),
                ),
                None => {
                    self.chat_tree
                        .remove_guild_from_guild_list(user_id, guild_id, "")?;
                    local_ids.push(member_id);
                }
            }
        }
        self.send_event_through_chan(
            EventSub::Homeserver,
            stream_event::Event::GuildRemovedFromList(stream_event::GuildRemovedFromList {
                guild_id,
                homeserver: String::new(),
            }),
            None,
            EventContext::new(local_ids),
        );

        Ok((DeleteGuildResponse {}).into_response())
    }

    #[rate(5, 5)]
    async fn delete_invite(
        &mut self,
        request: Request<DeleteInviteRequest>,
    ) -> ServerResult<Response<DeleteInviteResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let DeleteInviteRequest {
            guild_id,
            invite_id,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "invites.manage.delete", false)?;

        self.chat_tree
            .chat_tree
            .remove(&make_invite_key(invite_id.as_str()))
            .map_err(ServerError::DbError)?;

        Ok((DeleteInviteResponse {}).into_response())
    }

    #[rate(5, 5)]
    async fn delete_channel(
        &mut self,
        request: Request<DeleteChannelRequest>,
    ) -> ServerResult<Response<DeleteChannelResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let DeleteChannelRequest {
            guild_id,
            channel_id,
        } = request.into_message().await?;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree.check_perms(
            guild_id,
            Some(channel_id),
            user_id,
            "channels.manage.delete",
            false,
        )?;

        let channel_data = self
            .chat_tree
            .chat_tree
            .scan_prefix(&make_chan_key(guild_id, channel_id))
            .try_fold(Vec::new(), |mut all, res| {
                all.push(res.map_err(ServerError::from)?.0);
                ServerResult::Ok(all)
            })?;

        // Remove from ordering list
        let key = make_guild_chan_ordering_key(guild_id);
        let mut ordering = self.chat_tree.get_list_u64_logic(&key)?;
        if let Some(index) = ordering.iter().position(|oid| channel_id.eq(oid)) {
            ordering.remove(index);
        } else {
            unreachable!("all valid channel IDs are valid ordering IDs");
        }
        let serialized_ordering = self.chat_tree.serialize_list_u64_logic(ordering);

        let mut batch = Batch::default();
        for key in channel_data {
            batch.remove(key);
        }
        batch.insert(key, serialized_ordering);
        self.chat_tree
            .chat_tree
            .apply_batch(batch)
            .map_err(ServerError::DbError)?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::DeletedChannel(stream_event::ChannelDeleted {
                guild_id,
                channel_id,
            }),
            None,
            EventContext::empty(),
        );

        Ok((DeleteChannelResponse {}).into_response())
    }

    #[rate(5, 5)]
    async fn delete_message(
        &mut self,
        request: Request<DeleteMessageRequest>,
    ) -> ServerResult<Response<DeleteMessageResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let DeleteMessageRequest {
            guild_id,
            channel_id,
            message_id,
        } = request.into_message().await?;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        if self
            .chat_tree
            .get_message_logic(guild_id, channel_id, message_id)?
            .0
            .author_id
            != user_id
        {
            self.chat_tree.check_perms(
                guild_id,
                Some(channel_id),
                user_id,
                "messages.manage.delete",
                false,
            )?;
        }

        self.chat_tree
            .chat_tree
            .remove(&make_msg_key(guild_id, channel_id, message_id))
            .map_err(ServerError::DbError)?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::DeletedMessage(stream_event::MessageDeleted {
                guild_id,
                channel_id,
                message_id,
            }),
            Some(PermCheck::new(
                guild_id,
                Some(channel_id),
                "messages.view",
                false,
            )),
            EventContext::empty(),
        );

        Ok((DeleteMessageResponse {}).into_response())
    }

    #[rate(5, 5)]
    async fn join_guild(
        &mut self,
        request: Request<JoinGuildRequest>,
    ) -> ServerResult<Response<JoinGuildResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let JoinGuildRequest { invite_id } = request.into_message().await?;
        let key = make_invite_key(invite_id.as_str());

        let (guild_id, mut invite) = if let Some(raw) = self.chat_tree.get(&key)? {
            db::deser_invite_entry(raw)
        } else {
            return Err(ServerError::NoSuchInvite(invite_id.into()).into());
        };

        if self.chat_tree.is_user_banned_in_guild(guild_id, user_id)? {
            return Err(ServerError::UserBanned.into());
        }

        self.chat_tree
            .is_user_in_guild(guild_id, user_id)
            .ok()
            .map_or(Ok(()), |_| Err(ServerError::UserAlreadyInGuild))?;

        let is_infinite = invite.possible_uses == 0;

        if is_infinite.not() && invite.use_count >= invite.possible_uses {
            return Err(ServerError::InviteExpired.into());
        }

        self.chat_tree
            .insert(make_member_key(guild_id, user_id), [])?;
        self.chat_tree.add_default_role_to(guild_id, user_id)?;
        invite.use_count += 1;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::JoinedMember(stream_event::MemberJoined {
                guild_id,
                member_id: user_id,
            }),
            None,
            EventContext::empty(),
        );

        self.dispatch_guild_join(guild_id, user_id)?;

        let buf = rkyv_ser(&invite);
        self.chat_tree.insert(
            key,
            [guild_id.to_be_bytes().as_ref(), buf.as_ref()].concat(),
        )?;

        Ok((JoinGuildResponse { guild_id }).into_response())
    }

    #[rate(5, 5)]
    async fn leave_guild(
        &mut self,
        request: Request<LeaveGuildRequest>,
    ) -> ServerResult<Response<LeaveGuildResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let LeaveGuildRequest { guild_id } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        self.chat_tree
            .chat_tree
            .remove(&make_member_key(guild_id, user_id))
            .map_err(ServerError::DbError)?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::LeftMember(stream_event::MemberLeft {
                guild_id,
                member_id: user_id,
                leave_reason: LeaveReason::WillinglyUnspecified.into(),
            }),
            None,
            EventContext::empty(),
        );

        self.dispatch_guild_leave(guild_id, user_id)?;

        Ok((LeaveGuildResponse {}).into_response())
    }

    #[rate(20, 5)]
    async fn trigger_action(
        &mut self,
        _request: Request<TriggerActionRequest>,
    ) -> ServerResult<Response<TriggerActionResponse>> {
        Err(ServerError::NotImplemented.into())
    }

    #[rate(50, 10)]
    async fn send_message(
        &mut self,
        request: Request<SendMessageRequest>,
    ) -> ServerResult<Response<SendMessageResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let mut request = request.into_message().await?;
        let guild_id = request.guild_id;
        let channel_id = request.channel_id;
        let echo_id = request.echo_id;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree
            .check_perms(guild_id, Some(channel_id), user_id, "messages.send", false)?;

        let content = self
            .chat_tree
            .process_message_content(
                request.content.take(),
                self.media_root.as_path(),
                &self.host,
            )
            .await?;
        request.content = Some(content);
        let (message_id, message) = self.chat_tree.send_message_logic(user_id, request)?;

        let is_cmd_channel = self
            .chat_tree
            .get_admin_guild_keys()?
            .map(|(g, _, c)| (g, c))
            == Some((guild_id, channel_id));

        let action_content = if is_cmd_channel {
            if let Some(content::Content::TextMessage(content::TextContent {
                content: Some(FormattedText { text, .. }),
            })) = message.content.as_ref().and_then(|c| c.content.as_ref())
            {
                self.action_processor.run(text).ok()
            } else {
                None
            }
        } else {
            None
        };

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::SentMessage(Box::new(stream_event::MessageSent {
                echo_id,
                guild_id,
                channel_id,
                message_id,
                message: Some(message),
            })),
            Some(PermCheck::new(
                guild_id,
                Some(channel_id),
                "messages.view",
                false,
            )),
            EventContext::empty(),
        );

        if let Some(msg) = action_content {
            let content = Content {
                content: Some(content::Content::TextMessage(content::TextContent {
                    content: Some(FormattedText::new(msg, Vec::new())),
                })),
            };
            let (message_id, message) = self
                .chat_tree
                .send_with_system(guild_id, channel_id, content)?;
            self.send_event_through_chan(
                EventSub::Guild(guild_id),
                stream_event::Event::SentMessage(Box::new(stream_event::MessageSent {
                    echo_id,
                    guild_id,
                    channel_id,
                    message_id,
                    message: Some(message),
                })),
                Some(PermCheck::new(
                    guild_id,
                    Some(channel_id),
                    "messages.view",
                    false,
                )),
                EventContext::empty(),
            );
        }

        Ok((SendMessageResponse { message_id }).into_response())
    }

    #[rate(30, 5)]
    async fn query_has_permission(
        &mut self,
        request: Request<QueryHasPermissionRequest>,
    ) -> ServerResult<Response<QueryHasPermissionResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        self.chat_tree
            .query_has_permission_request(user_id, request.into_message().await?)
            .map(Response::new)
    }

    #[rate(10, 5)]
    async fn set_permissions(
        &mut self,
        request: Request<SetPermissionsRequest>,
    ) -> ServerResult<Response<SetPermissionsResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let SetPermissionsRequest {
            guild_id,
            channel_id,
            role_id,
            perms_to_give,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.check_perms(
            guild_id,
            channel_id,
            user_id,
            "permissions.manage.set",
            false,
        )?;

        // TODO: fix
        if !perms_to_give.is_empty() {
            self.chat_tree.set_permissions_logic(
                guild_id,
                channel_id,
                role_id,
                perms_to_give.clone(),
            )?;
            let members = self.chat_tree.get_guild_members_logic(guild_id)?.members;
            let guild_owners = self.chat_tree.get_guild_owners(guild_id)?;
            let for_users = members.iter().try_fold(
                Vec::with_capacity(members.len()),
                |mut all, user_id| {
                    if !guild_owners.contains(user_id) {
                        let maybe_user = self
                            .chat_tree
                            .get_user_roles_logic(guild_id, *user_id)?
                            .contains(&role_id)
                            .then(|| *user_id);
                        if let Some(user_id) = maybe_user {
                            all.push(user_id);
                        }
                    }
                    ServerResult::Ok(all)
                },
            )?;
            for perm in &perms_to_give {
                self.send_event_through_chan(
                    EventSub::Guild(guild_id),
                    stream_event::Event::PermissionUpdated(stream_event::PermissionUpdated {
                        guild_id,
                        channel_id,
                        query: perm.matches.clone(),
                        ok: perm.ok,
                    }),
                    None,
                    EventContext::new(for_users.clone()),
                );
            }
            self.send_event_through_chan(
                EventSub::Guild(guild_id),
                stream_event::Event::RolePermsUpdated(stream_event::RolePermissionsUpdated {
                    guild_id,
                    channel_id,
                    role_id,
                    new_perms: perms_to_give,
                }),
                Some(PermCheck {
                    guild_id,
                    channel_id: None,
                    check_for: "guild.manage",
                    must_be_guild_owner: false,
                }),
                EventContext::empty(),
            );
            Ok((SetPermissionsResponse {}).into_response())
        } else {
            Err(ServerError::NoPermissionsSpecified.into())
        }
    }

    #[rate(10, 5)]
    async fn get_permissions(
        &mut self,
        request: Request<GetPermissionsRequest>,
    ) -> ServerResult<Response<GetPermissionsResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let GetPermissionsRequest {
            guild_id,
            channel_id,
            role_id,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.check_perms(
            guild_id,
            channel_id,
            user_id,
            "permissions.manage.get",
            false,
        )?;

        let perms = self
            .chat_tree
            .get_permissions_logic(guild_id, channel_id, role_id)?
            .into_iter()
            .map(|(m, ok)| Permission {
                matches: m.into(),
                ok,
            })
            .collect();

        Ok((GetPermissionsResponse { perms }).into_response())
    }

    #[rate(10, 5)]
    async fn move_role(
        &mut self,
        request: Request<MoveRoleRequest>,
    ) -> ServerResult<Response<MoveRoleResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let MoveRoleRequest {
            guild_id,
            role_id,
            new_position,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "roles.manage", false)?;
        self.chat_tree.does_role_exist(guild_id, role_id)?;

        if let Some(pos) = new_position {
            self.chat_tree
                .move_role_logic(guild_id, role_id, Some(pos.clone()))?;
            self.send_event_through_chan(
                EventSub::Guild(guild_id),
                stream_event::Event::RoleMoved(stream_event::RoleMoved {
                    guild_id,
                    role_id,
                    new_position: Some(pos),
                }),
                None,
                EventContext::empty(),
            );
        }

        Ok((MoveRoleResponse {}).into_response())
    }

    #[rate(10, 5)]
    async fn get_guild_roles(
        &mut self,
        request: Request<GetGuildRolesRequest>,
    ) -> ServerResult<Response<GetGuildRolesResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let GetGuildRolesRequest { guild_id } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "roles.get", false)?;

        let roles = self.chat_tree.get_guild_roles_logic(guild_id)?;

        Ok((GetGuildRolesResponse { roles }).into_response())
    }

    #[rate(10, 5)]
    async fn add_guild_role(
        &mut self,
        request: Request<AddGuildRoleRequest>,
    ) -> ServerResult<Response<AddGuildRoleResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let AddGuildRoleRequest {
            guild_id,
            name,
            color,
            hoist,
            pingable,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "roles.manage", false)?;

        let role = Role {
            name: name.clone(),
            color,
            hoist,
            pingable,
        };
        let role_id = self.chat_tree.add_guild_role_logic(guild_id, None, role)?;
        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::RoleCreated(stream_event::RoleCreated {
                guild_id,
                role_id,
                name,
                color,
                hoist,
                pingable,
            }),
            None,
            EventContext::empty(),
        );

        Ok((AddGuildRoleResponse { role_id }).into_response())
    }

    #[rate(10, 5)]
    async fn modify_guild_role(
        &mut self,
        request: Request<ModifyGuildRoleRequest>,
    ) -> ServerResult<Response<ModifyGuildRoleResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let ModifyGuildRoleRequest {
            guild_id,
            role_id,
            new_name,
            new_color,
            new_hoist,
            new_pingable,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "roles.manage", false)?;

        let key = make_guild_role_key(guild_id, role_id);
        let mut role = if let Some(raw) = self.chat_tree.get(key)? {
            db::deser_role(raw)
        } else {
            return Err(ServerError::NoSuchRole { guild_id, role_id }.into());
        };

        if let Some(new_name) = new_name.clone() {
            role.name = new_name;
        }
        if let Some(new_color) = new_color {
            role.color = new_color;
        }
        if let Some(new_hoist) = new_hoist {
            role.hoist = new_hoist;
        }
        if let Some(new_pingable) = new_pingable {
            role.pingable = new_pingable;
        }

        let ser_role = rkyv_ser(&role);
        self.chat_tree.insert(key, ser_role)?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::RoleUpdated(stream_event::RoleUpdated {
                guild_id,
                role_id,
                new_name,
                new_color,
                new_hoist,
                new_pingable,
            }),
            None,
            EventContext::empty(),
        );

        Ok((ModifyGuildRoleResponse {}).into_response())
    }

    #[rate(10, 5)]
    async fn delete_guild_role(
        &mut self,
        request: Request<DeleteGuildRoleRequest>,
    ) -> ServerResult<Response<DeleteGuildRoleResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let DeleteGuildRoleRequest { guild_id, role_id } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "roles.manage", false)?;

        self.chat_tree
            .chat_tree
            .remove(&make_guild_role_key(guild_id, role_id))
            .map_err(ServerError::DbError)?
            .ok_or(ServerError::NoSuchRole { guild_id, role_id })?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::RoleDeleted(stream_event::RoleDeleted { guild_id, role_id }),
            None,
            EventContext::empty(),
        );

        Ok((DeleteGuildRoleResponse {}).into_response())
    }

    #[rate(10, 5)]
    async fn manage_user_roles(
        &mut self,
        request: Request<ManageUserRolesRequest>,
    ) -> ServerResult<Response<ManageUserRolesResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let ManageUserRolesRequest {
            guild_id,
            user_id: user_to_manage,
            give_role_ids,
            take_role_ids,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.is_user_in_guild(guild_id, user_to_manage)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "roles.user.manage", false)?;
        let user_to_manage = if user_to_manage != 0 {
            user_to_manage
        } else {
            user_id
        };

        let new_role_ids = self.chat_tree.manage_user_roles_logic(
            guild_id,
            user_to_manage,
            give_role_ids,
            take_role_ids,
        )?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::UserRolesUpdated(stream_event::UserRolesUpdated {
                guild_id,
                user_id: user_to_manage,
                new_role_ids,
            }),
            None,
            EventContext::empty(),
        );

        Ok((ManageUserRolesResponse {}).into_response())
    }

    #[rate(10, 5)]
    async fn get_user_roles(
        &mut self,
        request: Request<GetUserRolesRequest>,
    ) -> ServerResult<Response<GetUserRolesResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let GetUserRolesRequest {
            guild_id,
            user_id: user_to_fetch,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.is_user_in_guild(guild_id, user_to_fetch)?;
        let fetch_user = (user_to_fetch == 0)
            .then(|| user_id)
            .unwrap_or(user_to_fetch);
        if fetch_user != user_id {
            self.chat_tree
                .check_perms(guild_id, None, user_id, "roles.user.get", false)?;
        }

        let roles = self.chat_tree.get_user_roles_logic(guild_id, fetch_user)?;

        Ok((GetUserRolesResponse { roles }).into_response())
    }

    #[rate(4, 5)]
    async fn typing(
        &mut self,
        request: Request<TypingRequest>,
    ) -> ServerResult<Response<TypingResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let TypingRequest {
            guild_id,
            channel_id,
        } = request.into_message().await?;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree
            .check_perms(guild_id, Some(channel_id), user_id, "messages.send", false)?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::Typing(stream_event::Typing {
                user_id,
                guild_id,
                channel_id,
            }),
            Some(PermCheck::new(
                guild_id,
                Some(channel_id),
                "messages.view",
                false,
            )),
            EventContext::empty(),
        );

        Ok((TypingResponse {}).into_response())
    }

    #[rate(2, 5)]
    async fn preview_guild(
        &mut self,
        request: Request<PreviewGuildRequest>,
    ) -> ServerResult<Response<PreviewGuildResponse>> {
        let PreviewGuildRequest { invite_id } = request.into_message().await?;

        let key = make_invite_key(&invite_id);
        let guild_id = self
            .chat_tree
            .chat_tree
            .get(&key)
            .map_err(ServerError::DbError)?
            .ok_or_else(|| ServerError::NoSuchInvite(invite_id.into()))
            .map(|raw| db::deser_invite_entry_guild_id(&raw))?;
        let guild = self.chat_tree.get_guild_logic(guild_id)?;
        let member_count = self
            .chat_tree
            .chat_tree
            .scan_prefix(&make_guild_mem_prefix(guild_id))
            .try_fold(0, |all, res| {
                if let Err(err) = res {
                    return Err(ServerError::DbError(err).into());
                }
                ServerResult::Ok(all + 1)
            })?;

        Ok((PreviewGuildResponse {
            name: guild.name,
            picture: guild.picture,
            member_count,
        })
        .into_response())
    }

    async fn get_banned_users(
        &mut self,
        request: Request<GetBannedUsersRequest>,
    ) -> ServerResult<Response<GetBannedUsersResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let GetBannedUsersRequest { guild_id } = request.into_message().await?;

        let prefix = make_guild_banned_mem_prefix(guild_id);
        let banned_users = self.chat_tree.chat_tree.scan_prefix(&prefix).try_fold(
            Vec::new(),
            |mut all, res| {
                let (key, _) = res.map_err(ServerError::from)?;
                if key.len() == make_banned_member_key(0, 0).len() {
                    all.push(u64::from_be_bytes(unsafe {
                        key.split_at(prefix.len()).1.try_into().unwrap_unchecked()
                    }));
                }
                ServerResult::Ok(all)
            },
        )?;

        Ok((GetBannedUsersResponse { banned_users }).into_response())
    }

    #[rate(4, 5)]
    async fn ban_user(
        &mut self,
        request: Request<BanUserRequest>,
    ) -> ServerResult<Response<BanUserResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let BanUserRequest {
            guild_id,
            user_id: user_to_ban,
        } = request.into_message().await?;

        if user_id == user_to_ban {
            return Err(ServerError::CantKickOrBanYourself.into());
        }

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.is_user_in_guild(guild_id, user_to_ban)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "user.manage.ban", false)?;

        self.chat_tree.kick_user_logic(guild_id, user_to_ban)?;

        self.chat_tree.insert(
            make_banned_member_key(guild_id, user_to_ban),
            get_time_secs().to_be_bytes(),
        )?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::LeftMember(stream_event::MemberLeft {
                guild_id,
                member_id: user_to_ban,
                leave_reason: LeaveReason::Banned.into(),
            }),
            None,
            EventContext::empty(),
        );

        self.dispatch_guild_leave(guild_id, user_to_ban)?;

        Ok((BanUserResponse {}).into_response())
    }

    #[rate(4, 5)]
    async fn kick_user(
        &mut self,
        request: Request<KickUserRequest>,
    ) -> ServerResult<Response<KickUserResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let KickUserRequest {
            guild_id,
            user_id: user_to_kick,
        } = request.into_message().await?;

        if user_id == user_to_kick {
            return Err(ServerError::CantKickOrBanYourself.into());
        }

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.is_user_in_guild(guild_id, user_to_kick)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "user.manage.kick", false)?;

        self.chat_tree.kick_user_logic(guild_id, user_to_kick)?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::LeftMember(stream_event::MemberLeft {
                guild_id,
                member_id: user_to_kick,
                leave_reason: LeaveReason::Kicked.into(),
            }),
            None,
            EventContext::empty(),
        );

        self.dispatch_guild_leave(guild_id, user_to_kick)?;

        Ok((KickUserResponse {}).into_response())
    }

    #[rate(4, 5)]
    async fn unban_user(
        &mut self,
        request: Request<UnbanUserRequest>,
    ) -> ServerResult<Response<UnbanUserResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let UnbanUserRequest {
            guild_id,
            user_id: user_to_unban,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "user.manage.unban", false)?;

        self.chat_tree
            .remove(make_banned_member_key(guild_id, user_to_unban))?;

        Ok((UnbanUserResponse {}).into_response())
    }

    async fn get_pinned_messages(
        &mut self,
        _request: Request<GetPinnedMessagesRequest>,
    ) -> ServerResult<Response<GetPinnedMessagesResponse>> {
        Err(ServerError::NotImplemented.into())
    }

    async fn pin_message(
        &mut self,
        _request: Request<PinMessageRequest>,
    ) -> ServerResult<Response<PinMessageResponse>> {
        Err(ServerError::NotImplemented.into())
    }

    async fn unpin_message(
        &mut self,
        _request: Request<UnpinMessageRequest>,
    ) -> ServerResult<Response<UnpinMessageResponse>> {
        Err(ServerError::NotImplemented.into())
    }

    fn stream_events_on_upgrade(
        &mut self,
        response: http::Response<BoxBody>,
    ) -> http::Response<BoxBody> {
        set_proto_name(response)
    }

    #[rate(1, 5)]
    async fn stream_events(
        &mut self,
        request: Request<()>,
        socket: Socket<StreamEventsRequest, StreamEventsResponse>,
    ) -> Result<(), HrpcServerError> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;
        tracing::debug!("stream events validated for user {}", user_id);

        tracing::debug!("creating stream events for user {}", user_id);
        let (sub_tx, sub_rx) = mpsc::channel(64);
        let chat_tree = self.chat_tree.clone();

        let send_loop = self.spawn_event_stream_processor(user_id, sub_rx, socket.clone());
        let recv_loop = async move {
            loop {
                let req = bail_result!(socket.receive_message().await);
                if let Some(req) = req.request {
                    use stream_events_request::*;

                    tracing::debug!("got new stream events request for user {}", user_id);

                    let sub = match req {
                        Request::SubscribeToGuild(SubscribeToGuild { guild_id }) => {
                            match chat_tree.check_guild_user(guild_id, user_id) {
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
                    };

                    drop(sub_tx.send(sub).await);
                }
            }
            #[allow(unreachable_code)]
            ServerResult::Ok(())
        };

        tokio::select!(
            res = send_loop => {
                res.unwrap();
            }
            res = tokio::spawn(recv_loop) => {
                res.unwrap()?;
            }
        );
        tracing::debug!("stream events ended for user {}", user_id);

        Ok(())
    }

    #[rate(10, 5)]
    async fn add_reaction(
        &mut self,
        request: Request<AddReactionRequest>,
    ) -> ServerResult<Response<AddReactionResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let AddReactionRequest {
            guild_id,
            channel_id,
            message_id,
            emote,
        } = request.into_message().await?;

        if let Some(emote) = emote {
            self.chat_tree.check_perms(
                guild_id,
                Some(channel_id),
                user_id,
                all_permissions::MESSAGES_REACTIONS_ADD,
                false,
            )?;

            let reaction = self
                .chat_tree
                .update_reaction(user_id, guild_id, channel_id, message_id, emote, true)?;
            self.send_reaction_event(guild_id, channel_id, message_id, reaction);
        }

        Ok((AddReactionResponse {}).into_response())
    }

    #[rate(10, 5)]
    async fn remove_reaction(
        &mut self,
        request: Request<RemoveReactionRequest>,
    ) -> ServerResult<Response<RemoveReactionResponse>> {
        #[allow(unused_variables)]
        let user_id = self.valid_sessions.auth(&request)?;

        let RemoveReactionRequest {
            guild_id,
            channel_id,
            message_id,
            emote,
        } = request.into_message().await?;

        if let Some(emote) = emote {
            self.chat_tree.check_perms(
                guild_id,
                Some(channel_id),
                user_id,
                all_permissions::MESSAGES_REACTIONS_REMOVE,
                false,
            )?;

            let reaction = self
                .chat_tree
                .update_reaction(user_id, guild_id, channel_id, message_id, emote, false)?;
            if reaction.is_some() {
                self.send_reaction_event(guild_id, channel_id, message_id, reaction);
            }
        }

        Ok((RemoveReactionResponse {}).into_response())
    }

    #[rate(2, 60)]
    async fn grant_ownership(
        &mut self,
        request: Request<GrantOwnershipRequest>,
    ) -> ServerResult<Response<GrantOwnershipResponse>> {
        let user_id = self.valid_sessions.auth(&request)?;

        let GrantOwnershipRequest {
            new_owner_id,
            guild_id,
        } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.is_user_in_guild(guild_id, new_owner_id)?;

        self.chat_tree
            .check_perms(guild_id, None, user_id, "", true)?;

        let mut guild = self.chat_tree.get_guild_logic(guild_id)?;
        guild.owner_ids.push(new_owner_id);
        self.chat_tree.put_guild_logic(guild_id, guild)?;

        Ok((GrantOwnershipResponse {}).into_response())
    }

    #[rate(2, 60)]
    async fn give_up_ownership(
        &mut self,
        request: Request<GiveUpOwnershipRequest>,
    ) -> ServerResult<Response<GiveUpOwnershipResponse>> {
        let user_id = self.valid_sessions.auth(&request)?;

        let GiveUpOwnershipRequest { guild_id } = request.into_message().await?;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        self.chat_tree
            .check_perms(guild_id, None, user_id, "", true)?;

        let mut guild = self.chat_tree.get_guild_logic(guild_id)?;
        if guild.owner_ids.len() > 1 {
            if let Some(pos) = guild.owner_ids.iter().position(|id| user_id.eq(id)) {
                guild.owner_ids.remove(pos);
            }
        } else {
            return Err(ServerError::MustNotBeLastOwner.into());
        }

        Ok((GiveUpOwnershipResponse {}).into_response())
    }

    async fn create_room(
        &mut self,
        request: Request<CreateRoomRequest>,
    ) -> ServerResult<Response<CreateRoomResponse>> {
        Err(ServerError::NotImplemented.into())
    }

    async fn create_direct_message(
        &mut self,
        request: Request<CreateDirectMessageRequest>,
    ) -> ServerResult<Response<CreateDirectMessageResponse>> {
        Err(ServerError::NotImplemented.into())
    }

    async fn upgrade_room_to_guild(
        &mut self,
        request: Request<v1::UpgradeRoomToGuildRequest>,
    ) -> ServerResult<Response<v1::UpgradeRoomToGuildResponse>> {
        Err(ServerError::NotImplemented.into())
    }

    async fn invite_user_to_guild(
        &mut self,
        request: Request<v1::InviteUserToGuildRequest>,
    ) -> ServerResult<Response<v1::InviteUserToGuildResponse>> {
        Err(ServerError::NotImplemented.into())
    }

    async fn get_pending_invites(
        &mut self,
        request: Request<v1::GetPendingInvitesRequest>,
    ) -> ServerResult<Response<v1::GetPendingInvitesResponse>> {
        Err(ServerError::NotImplemented.into())
    }

    async fn reject_pending_invite(
        &mut self,
        request: Request<v1::RejectPendingInviteRequest>,
    ) -> ServerResult<Response<v1::RejectPendingInviteResponse>> {
        Err(ServerError::NotImplemented.into())
    }

    async fn ignore_pending_invite(
        &mut self,
        request: Request<v1::IgnorePendingInviteRequest>,
    ) -> ServerResult<Response<v1::IgnorePendingInviteResponse>> {
        Err(ServerError::NotImplemented.into())
    }
}

#[derive(Clone)]
pub struct ChatTree {
    pub chat_tree: db::ArcTree,
}

impl ChatTree {
    impl_db_methods!(chat_tree);

    pub fn new(db: &dyn Db) -> DbResult<Self> {
        let chat_tree = db.open_tree(b"chat")?;
        Ok(Self { chat_tree })
    }

    pub fn is_user_in_guild(&self, guild_id: u64, user_id: u64) -> ServerResult<()> {
        self.contains_key(&make_member_key(guild_id, user_id))?
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::UserNotInGuild { guild_id, user_id }))
            .map_err(Into::into)
    }

    pub fn does_guild_exist(&self, guild_id: u64) -> ServerResult<()> {
        self.contains_key(&guild_id.to_be_bytes())?
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::NoSuchGuild(guild_id)))
            .map_err(Into::into)
    }

    pub fn does_channel_exist(&self, guild_id: u64, channel_id: u64) -> ServerResult<()> {
        self.contains_key(&make_chan_key(guild_id, channel_id))?
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::NoSuchChannel {
                guild_id,
                channel_id,
            }))
            .map_err(Into::into)
    }

    pub fn does_role_exist(&self, guild_id: u64, role_id: u64) -> ServerResult<()> {
        self.contains_key(&make_guild_role_key(guild_id, role_id))?
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::NoSuchRole { guild_id, role_id }))
            .map_err(Into::into)
    }

    pub fn is_user_banned_in_guild(&self, guild_id: u64, user_id: u64) -> ServerResult<bool> {
        self.contains_key(&make_banned_member_key(guild_id, user_id))
    }

    pub fn get_guild_owners(&self, guild_id: u64) -> ServerResult<Vec<u64>> {
        self.get(guild_id.to_be_bytes().as_ref())?
            .map_or_else(
                || Err(ServerError::NoSuchGuild(guild_id)),
                |raw| Ok(db::deser_guild(raw).owner_ids),
            )
            .map_err(Into::into)
    }

    pub fn is_user_guild_owner(&self, guild_id: u64, user_id: u64) -> ServerResult<bool> {
        Ok(self
            .get_guild_owners(guild_id)?
            .into_iter()
            .any(|owner| owner == user_id))
    }

    pub fn check_guild_user_channel(
        &self,
        guild_id: u64,
        user_id: u64,
        channel_id: u64,
    ) -> ServerResult<()> {
        self.check_guild_user(guild_id, user_id)?;
        self.does_channel_exist(guild_id, channel_id)
    }

    pub fn check_guild_user(&self, guild_id: u64, user_id: u64) -> ServerResult<()> {
        self.check_guild(guild_id)?;
        self.is_user_in_guild(guild_id, user_id)
    }

    #[inline(always)]
    pub fn check_guild(&self, guild_id: u64) -> ServerResult<()> {
        self.does_guild_exist(guild_id)
    }

    pub fn get_message_logic(
        &self,
        guild_id: u64,
        channel_id: u64,
        message_id: u64,
    ) -> ServerResult<(HarmonyMessage, [u8; 26])> {
        let key = make_msg_key(guild_id, channel_id, message_id);

        let message = if let Some(msg) = self.get(&key)? {
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

    pub fn get_guild_logic(&self, guild_id: u64) -> ServerResult<Guild> {
        let guild = if let Some(guild_raw) = self.get(guild_id.to_be_bytes().as_ref())? {
            db::deser_guild(guild_raw)
        } else {
            return Err(ServerError::NoSuchGuild(guild_id).into());
        };

        Ok(guild)
    }

    pub fn put_guild_logic(&self, guild_id: u64, guild: Guild) -> ServerResult<()> {
        let buf = rkyv_ser(&guild);
        self.insert(guild_id.to_be_bytes(), buf).map(|_| ())
    }

    pub fn get_guild_invites_logic(&self, guild_id: u64) -> ServerResult<GetGuildInvitesResponse> {
        let invites = self
            .scan_prefix(INVITE_PREFIX)
            .try_fold(Vec::new(), |mut all, res| {
                let (key, value) = res?;
                let (inv_guild_id_raw, invite_raw) = value.split_at(size_of::<u64>());
                // Safety: this unwrap cannot fail since we split at u64 boundary
                let inv_guild_id =
                    u64::from_be_bytes(unsafe { inv_guild_id_raw.try_into().unwrap_unchecked() });
                if guild_id == inv_guild_id {
                    let invite_id = unsafe {
                        std::str::from_utf8_unchecked(key.split_at(INVITE_PREFIX.len()).1)
                    };
                    let invite = db::deser_invite(invite_raw.into());
                    all.push(InviteWithId {
                        invite_id: invite_id.to_string(),
                        invite: Some(invite),
                    });
                }
                ServerResult::Ok(all)
            })?;

        Ok(GetGuildInvitesResponse { invites })
    }

    pub fn get_guild_members_logic(&self, guild_id: u64) -> ServerResult<GetGuildMembersResponse> {
        let prefix = make_guild_mem_prefix(guild_id);
        let members = self
            .scan_prefix(&prefix)
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

    pub fn get_guild_channels_logic(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> ServerResult<GetGuildChannelsResponse> {
        let prefix = make_guild_chan_prefix(guild_id);
        let mut channels = self
            .scan_prefix(&prefix)
            .try_fold(Vec::new(), |mut all, res| {
                let (key, value) = res?;
                if key.len() == prefix.len() + size_of::<u64>() {
                    let channel_id = u64::from_be_bytes(
                        // Safety: this unwrap is safe since we check if it's a valid u64 beforehand
                        unsafe { key.split_at(prefix.len()).1.try_into().unwrap_unchecked() },
                    );
                    let allowed = self
                        .check_perms(guild_id, Some(channel_id), user_id, "messages.view", false)
                        .is_ok();
                    if allowed {
                        // Safety: this unwrap is safe since we only store valid Channel message
                        let channel = db::deser_chan(value);
                        all.push(ChannelWithId {
                            channel_id,
                            channel: Some(channel),
                        });
                    }
                }
                ServerResult::Ok(all)
            })?;

        if channels.is_empty() {
            return Ok(GetGuildChannelsResponse {
                channels: Vec::new(),
            });
        }

        let ordering_raw = self
            .chat_tree
            .get(&make_guild_chan_ordering_key(guild_id))
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
    pub fn get_list_u64_logic(&self, key: &[u8]) -> ServerResult<Vec<u64>> {
        Ok(db::make_u64_iter_logic(
            self.chat_tree
                .get(key)
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

    pub fn move_role_logic(
        &self,
        guild_id: u64,
        role_id: u64,
        position: Option<ItemPosition>,
    ) -> ServerResult<()> {
        self.update_order_logic(
            role_id,
            position,
            |role_id| self.does_role_exist(guild_id, role_id),
            &make_guild_role_ordering_key(guild_id),
        )
    }

    pub fn update_channel_order_logic(
        &self,
        guild_id: u64,
        channel_id: u64,
        position: Option<ItemPosition>,
    ) -> ServerResult<()> {
        self.update_order_logic(
            channel_id,
            position,
            |channel_id| self.does_channel_exist(guild_id, channel_id),
            &make_guild_chan_ordering_key(guild_id),
        )
    }

    #[inline(always)]
    pub fn update_order_logic(
        &self,
        id: u64,
        position: Option<ItemPosition>,
        check_exists: impl Fn(u64) -> ServerResult<()>,
        key: &[u8],
    ) -> ServerResult<()> {
        let mut ordering = self.get_list_u64_logic(key)?;

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
            check_exists(item_id)?;
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
        self.insert(key, serialized_ordering)?;

        Ok(())
    }

    pub fn get_channel_messages_logic(
        &self,
        guild_id: u64,
        channel_id: u64,
        message_id: u64,
        direction: Option<Direction>,
        count: Option<u32>,
    ) -> ServerResult<GetChannelMessagesResponse> {
        let direction = direction.unwrap_or_default();

        let prefix = make_msg_prefix(guild_id, channel_id);
        let get_last_message_id = || {
            self.chat_tree
                .scan_prefix(&prefix)
                .last()
                .map(|last| {
                    last.map(|last| {
                        u64::from_be_bytes(
                            // Safety: cannot fail since the remainder after we split is a valid u64
                            unsafe {
                                last.0
                                    .split_at(prefix.len())
                                    .1
                                    .try_into()
                                    .unwrap_unchecked()
                            },
                        )
                    })
                })
                .transpose()
                .map_err(ServerError::DbError)
        };

        let get_messages = |to: u64| {
            let count = count.map_or_else(
                || {
                    matches!(direction, Direction::Around)
                        .then(|| 12)
                        .unwrap_or(25)
                },
                |c| c as u64,
            );
            let last_message_id = get_last_message_id()?.unwrap_or(1);

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

            ServerResult::Ok(GetChannelMessagesResponse {
                reached_top: from == 1,
                reached_bottom: to == last_message_id,
                messages,
            })
        };

        (message_id == 0)
            .then(|| {
                get_last_message_id()?.map_or_else(
                    || {
                        Ok(GetChannelMessagesResponse {
                            reached_top: true,
                            reached_bottom: true,
                            messages: Vec::new(),
                        })
                    },
                    |last| get_messages(last + 1),
                )
            })
            .unwrap_or_else(|| get_messages(message_id))
    }

    pub fn get_user_roles_logic(&self, guild_id: u64, user_id: u64) -> ServerResult<Vec<u64>> {
        let key = make_guild_user_roles_key(guild_id, user_id);
        Ok(self
            .chat_tree
            .get(&key)
            .map_err(ServerError::from)?
            .map_or_else(Vec::default, |raw| {
                raw.chunks_exact(size_of::<u64>())
                    // Safety: this is safe since we split at u64 boundary
                    .map(|raw| u64::from_be_bytes(unsafe { raw.try_into().unwrap_unchecked() }))
                    .collect()
            }))
    }

    pub fn query_has_permission_logic(
        &self,
        guild_id: u64,
        channel_id: Option<u64>,
        user_id: u64,
        check_for: &str,
    ) -> ServerResult<bool> {
        let key = make_guild_user_roles_key(guild_id, user_id);
        let user_roles = self.get_list_u64_logic(&key)?;

        if let Some(channel_id) = channel_id {
            for role_id in &user_roles {
                let perms = self.get_permissions_logic(guild_id, Some(channel_id), *role_id)?;
                let is_allowed =
                    has_permission(perms.iter().map(|(m, ok)| (m.as_str(), *ok)), check_for);
                if let Some(true) = is_allowed {
                    return Ok(true);
                }
            }
        }

        for role_id in user_roles {
            let perms = self.get_permissions_logic(guild_id, None, role_id)?;
            let is_allowed =
                has_permission(perms.iter().map(|(m, ok)| (m.as_str(), *ok)), check_for);
            if let Some(true) = is_allowed {
                return Ok(true);
            }
        }

        Ok(false)
    }

    pub fn check_perms(
        &self,
        guild_id: u64,
        channel_id: Option<u64>,
        user_id: u64,
        check_for: &str,
        must_be_guild_owner: bool,
    ) -> ServerResult<()> {
        let is_owner = self.is_user_guild_owner(guild_id, user_id)?;
        if must_be_guild_owner {
            if is_owner {
                return Ok(());
            }
        } else if is_owner
            || self.query_has_permission_logic(guild_id, channel_id, user_id, check_for)?
        {
            return Ok(());
        }
        Err((ServerError::NotEnoughPermissions {
            must_be_guild_owner,
            missing_permission: check_for.into(),
        })
        .into())
    }

    pub fn kick_user_logic(&self, guild_id: u64, user_id: u64) -> ServerResult<()> {
        let mut batch = Batch::default();
        batch.remove(make_member_key(guild_id, user_id));
        batch.remove(make_guild_user_roles_key(guild_id, user_id));
        self.chat_tree
            .apply_batch(batch)
            .map_err(ServerError::DbError)
            .map_err(Into::into)
    }

    pub fn manage_user_roles_logic(
        &self,
        guild_id: u64,
        user_id: u64,
        give_role_ids: Vec<u64>,
        take_role_ids: Vec<u64>,
    ) -> ServerResult<Vec<u64>> {
        let mut roles = self.get_user_roles_logic(guild_id, user_id)?;
        for role_id in give_role_ids {
            self.does_role_exist(guild_id, role_id)?;
            roles.push(role_id);
        }
        for role_id in take_role_ids {
            self.does_role_exist(guild_id, role_id)?;
            if let Some(index) = roles.iter().position(|oid| role_id.eq(oid)) {
                roles.remove(index);
            }
        }

        let key = make_guild_user_roles_key(guild_id, user_id);
        let ser_roles = self.serialize_list_u64_logic(roles.clone());
        self.insert(key, ser_roles)?;

        Ok(roles)
    }

    pub fn add_default_role_to(&self, guild_id: u64, user_id: u64) -> ServerResult<()> {
        self.manage_user_roles_logic(guild_id, user_id, vec![DEFAULT_ROLE_ID], Vec::new())
            .map(|_| ())
    }

    pub fn add_guild_role_logic(
        &self,
        guild_id: u64,
        role_id: Option<u64>,
        role: Role,
    ) -> ServerResult<u64> {
        let (role_id, key) = if let Some(id) = role_id {
            (id, make_guild_role_key(guild_id, id))
        } else {
            let mut rng = rand::thread_rng();
            let mut role_id = rng.gen_range(1..u64::MAX);
            let mut key = make_guild_role_key(guild_id, role_id);
            while self.contains_key(&key)? {
                role_id = rng.gen_range(1..u64::MAX);
                key = make_guild_role_key(guild_id, role_id);
            }
            (role_id, key)
        };
        let ser_role = rkyv_ser(&role);
        self.insert(key, ser_role)?;
        self.move_role_logic(guild_id, role_id, None)?;
        Ok(role_id)
    }

    pub fn set_permissions_logic(
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
            .map_err(ServerError::from)?;
        Ok(())
    }

    pub fn create_channel_logic(
        &self,
        guild_id: u64,
        channel_name: String,
        kind: ChannelKind,
        metadata: Option<Metadata>,
        position: Option<ItemPosition>,
    ) -> ServerResult<u64> {
        let channel_id = {
            let mut rng = rand::thread_rng();
            let mut channel_id = rng.gen_range(1..=u64::MAX);
            let mut key = make_chan_key(guild_id, channel_id);
            while self.contains_key(&key)? {
                channel_id = rng.gen_range(1..=u64::MAX);
                key = make_chan_key(guild_id, channel_id);
            }
            channel_id
        };
        let key = make_chan_key(guild_id, channel_id);

        let channel = Channel {
            metadata,
            channel_name,
            kind: kind.into(),
        };
        let buf = rkyv_ser(&channel);
        self.insert(key, buf)?;

        // Add from ordering list
        self.update_channel_order_logic(guild_id, channel_id, position)?;

        Ok(channel_id)
    }

    pub fn create_guild_logic(
        &self,
        user_id: u64,
        name: String,
        picture: Option<String>,
        metadata: Option<Metadata>,
    ) -> ServerResult<u64> {
        let guild_id = {
            let mut rng = rand::thread_rng();
            let mut guild_id = rng.gen_range(1..u64::MAX);
            while self.contains_key(&guild_id.to_be_bytes())? {
                guild_id = rng.gen_range(1..u64::MAX);
            }
            guild_id
        };

        let guild = Guild {
            name,
            picture,
            owner_ids: vec![user_id],
            metadata,
            kind: Some(GuildKind {
                kind: Some(guild_kind::Kind::new_normal(guild_kind::Normal::new())),
            }),
        };
        let buf = rkyv_ser(&guild);

        self.insert(guild_id.to_be_bytes(), buf)?;
        if user_id != 0 {
            self.insert(make_member_key(guild_id, user_id), [])?;
        }

        // Some basic default setup
        let everyone_role_id = self.add_guild_role_logic(
            guild_id,
            // "everyone" role must have id 0 according to protocol
            Some(DEFAULT_ROLE_ID),
            Role {
                name: "everyone".to_string(),
                pingable: false,
                ..Default::default()
            },
        )?;
        if user_id != 0 {
            self.add_default_role_to(guild_id, user_id)?;
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
        self.set_permissions_logic(guild_id, None, everyone_role_id, def_perms.clone())?;
        let channel_id = self.create_channel_logic(
            guild_id,
            "general".to_string(),
            ChannelKind::TextUnspecified,
            None,
            None,
        )?;

        self.set_permissions_logic(guild_id, Some(channel_id), everyone_role_id, def_perms)?;

        Ok(guild_id)
    }

    pub fn create_invite_logic(
        &self,
        guild_id: u64,
        name: &str,
        possible_uses: u32,
    ) -> ServerResult<()> {
        let key = make_invite_key(name);

        if name.is_empty() {
            return Err(ServerError::InviteNameEmpty.into());
        }

        if self.get(&key)?.is_some() {
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
        )?;

        Ok(())
    }

    /// Calculates all users which can "see" the given user
    pub fn calculate_users_seeing_user(&self, user_id: u64) -> ServerResult<Vec<u64>> {
        let prefix = make_guild_list_key_prefix(user_id);
        self.scan_prefix(&prefix)
            .try_fold(Vec::new(), |mut all, res| {
                let (key, _) = res?;
                let (_, guild_id_raw) = key.split_at(prefix.len());
                let (id_raw, _) = guild_id_raw.split_at(size_of::<u64>());
                // Safety: safe since we split at u64 boundary
                let guild_id = u64::from_be_bytes(unsafe { id_raw.try_into().unwrap_unchecked() });
                let mut members = self.get_guild_members_logic(guild_id)?.members;
                all.append(&mut members);
                Ok(all)
            })
    }

    /// Adds a guild to a user's guild list
    pub fn add_guild_to_guild_list(
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
        )?;
        Ok(())
    }

    /// Removes a guild from a user's guild list
    pub fn remove_guild_from_guild_list(
        &self,
        user_id: u64,
        guild_id: u64,
        homeserver: &str,
    ) -> ServerResult<()> {
        self.chat_tree
            .remove(&make_guild_list_key(user_id, guild_id, homeserver))
            .map(|_| ())
            .map_err(ServerError::from)
            .map_err(Into::into)
    }

    pub fn get_guild_roles_logic(&self, guild_id: u64) -> ServerResult<Vec<RoleWithId>> {
        let prefix = make_guild_role_prefix(guild_id);
        self.chat_tree
            .scan_prefix(&prefix)
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

    pub fn get_permissions_logic(
        &self,
        guild_id: u64,
        channel_id: Option<u64>,
        role_id: u64,
    ) -> ServerResult<Vec<(SmolStr, bool)>> {
        let get = |prefix: &[u8]| {
            self.chat_tree
                .scan_prefix(prefix)
                .try_fold(Vec::new(), |mut all, res| {
                    let (key, value) = res.map_err(ServerError::from)?;
                    let matches_raw = key.split_at(prefix.len()).1;
                    let matches = unsafe { std::str::from_utf8_unchecked(matches_raw) };
                    let ok = value[0] != 0;
                    all.push((matches.into(), ok));
                    Ok(all)
                })
        };

        if let Some(channel_id) = channel_id {
            let prefix = make_role_channel_perms_prefix(guild_id, channel_id, role_id);
            get(&prefix)
        } else {
            let prefix = make_role_guild_perms_prefix(guild_id, role_id);
            get(&prefix)
        }
    }

    pub fn query_has_permission_request(
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

        self.check_guild_user(guild_id, user_id)?;

        let check_as = r#as.unwrap_or(user_id);

        if r#as.is_some() {
            self.check_perms(guild_id, channel_id, user_id, "permissions.query", false)?;
        }

        if check_for.is_empty() {
            return Err(ServerError::EmptyPermissionQuery.into());
        }

        Ok(QueryHasPermissionResponse {
            ok: self
                .check_perms(guild_id, channel_id, check_as, &check_for, false)
                .is_ok(),
        })
    }

    pub fn get_next_message_id(&self, guild_id: u64, channel_id: u64) -> ServerResult<u64> {
        let msg_prefix = make_msg_prefix(guild_id, channel_id);
        self.chat_tree
            .scan_prefix(&msg_prefix)
            .last()
            // Ensure that the first message ID is always 1!
            .map_or(Ok(1), |res| {
                // Safety: this won't cause UB since we only store u64 after the prefix [ref:msg_key_u64]
                res.map(|res| {
                    u64::from_be_bytes(unsafe {
                        res.0
                            .split_at(msg_prefix.len())
                            .1
                            .try_into()
                            .unwrap_unchecked()
                    }) + 1
                })
                .map_err(ServerError::DbError)
                .map_err(Into::into)
            })
    }

    pub fn send_message_logic(
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

        let message_id = self.get_next_message_id(guild_id, channel_id)?;
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
        self.insert(key, value)?;

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
                    if embed.embed.is_none() {
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

    pub fn get_admin_guild_keys(&self) -> ServerResult<Option<(u64, u64, u64)>> {
        Ok(self.get(ADMIN_GUILD_KEY)?.map(|raw| {
            let (gid_raw, rest) = raw.split_at(size_of::<u64>());
            let guild_id = unsafe { u64::from_be_bytes(gid_raw.try_into().unwrap_unchecked()) };
            let (log_raw, cmd_raw) = rest.split_at(size_of::<u64>());
            let log_id = unsafe { u64::from_be_bytes(log_raw.try_into().unwrap_unchecked()) };
            let cmd_id = unsafe { u64::from_be_bytes(cmd_raw.try_into().unwrap_unchecked()) };
            (guild_id, log_id, cmd_id)
        }))
    }

    pub fn set_admin_guild_keys(
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
        self.insert(ADMIN_GUILD_KEY, value)?;
        Ok(())
    }

    pub fn send_with_system(
        &self,
        guild_id: u64,
        channel_id: u64,
        content: Content,
    ) -> ServerResult<(u64, HarmonyMessage)> {
        let request = SendMessageRequest::default()
            .with_guild_id(guild_id)
            .with_channel_id(channel_id)
            .with_content(content)
            .with_overrides(Overrides {
                username: Some("System".to_string()),
                reason: Some(overrides::Reason::SystemMessage(Empty {})),
                avatar: None,
            });
        self.send_message_logic(0, request)
    }

    pub fn update_reaction(
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
            .map_err(ServerError::from)?;
        if matches!((add, reacted), (true, true) | (false, false)) {
            return Ok(None);
        }

        // TODO: validate the emote image_id is below a certain size
        self.check_guild_user_channel(guild_id, user_id, channel_id)?;

        let (mut message, message_key) =
            self.get_message_logic(guild_id, channel_id, message_id)?;

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
            .map_err(ServerError::from)?;

        Ok(reaction)
    }
}

use std::fmt::{self, Debug};
use tracing::Subscriber;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields};
use tracing_subscriber::registry::LookupSpan;

pub struct AdminLogChannelLogger {
    inner: Option<(ChatTree, EventSender)>,
}

impl AdminLogChannelLogger {
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            inner: Some((deps.chat_tree.clone(), deps.chat_event_sender.clone())),
        }
    }

    pub fn empty() -> Self {
        Self { inner: None }
    }
}

impl<S, N> FormatEvent<S, N> for AdminLogChannelLogger
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        _writer: &mut dyn fmt::Write,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        use tracing::Level;

        let metadata = event.metadata();
        if Level::TRACE.eq(metadata.level()) {
            return Ok(());
        }

        if let Some((chat_tree, chat_event_sender)) = &self.inner {
            if let Some((guild_id, channel_id, _)) = chat_tree.get_admin_guild_keys().unwrap() {
                let color = match *metadata.level() {
                    Level::DEBUG => Some(color::encode_rgb([0, 0, 220])),
                    Level::INFO => Some(color::encode_rgb([0, 220, 0])),
                    Level::WARN => Some(color::encode_rgb([220, 220, 0])),
                    Level::ERROR => Some(color::encode_rgb([220, 0, 0])),
                    _ => None,
                };

                let mut embed_fields = Vec::new();
                ctx.visit_spans(|span| {
                    let ext = span.extensions();
                    let fields = ext.get::<FormattedFields<N>>().unwrap();
                    if !fields.is_empty() {
                        let split = fields.split(' ').collect::<Vec<_>>();
                        let split_len = split.len();
                        let fields = split.into_iter().enumerate().fold(
                            String::new(),
                            |mut tot, (index, item)| {
                                tot.push_str(item);
                                if index + 1 != split_len {
                                    tot.push('\n');
                                }
                                tot
                            },
                        );
                        embed_fields.push(embed::EmbedField {
                            title: format!("{}:", span.name()),
                            body: Some(FormattedText::new(fields, Vec::new())),
                            ..Default::default()
                        });
                    }
                    Ok(())
                })?;

                let mut body_text = String::new();

                ctx.field_format().format_fields(&mut body_text, event)?;

                let content = Content {
                    content: Some(content::Content::EmbedMessage(content::EmbedContent {
                        embed: Some(Box::new(Embed {
                            title: format!("{} {}:", metadata.level(), metadata.target()),
                            body: Some(FormattedText::new(body_text, Vec::new())),
                            fields: embed_fields,
                            color,
                            ..Default::default()
                        })),
                    })),
                };
                let (message_id, message) = chat_tree
                    .send_with_system(guild_id, channel_id, content)
                    .unwrap();

                let _ = chat_event_sender.send(Arc::new(EventBroadcast::new(
                    EventSub::Guild(guild_id),
                    Event::Chat(stream_event::Event::SentMessage(Box::new(
                        stream_event::MessageSent {
                            echo_id: None,
                            guild_id,
                            channel_id,
                            message_id,
                            message: Some(message),
                        },
                    ))),
                    Some(PermCheck::new(
                        guild_id,
                        Some(channel_id),
                        "messages.view",
                        false,
                    )),
                    EventContext::empty(),
                )));
            }
        }

        Ok(())
    }
}
