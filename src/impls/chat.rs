use std::{
    collections::HashSet, convert::TryInto, io::BufReader, mem::size_of, ops::Not, path::PathBuf,
    str::FromStr,
};

use harmony_rust_sdk::api::{
    chat::{
        get_channel_messages_request::Direction, permission::has_permission, stream_event,
        Message as HarmonyMessage, *,
    },
    exports::hrpc::{
        return_print,
        server::{ServerError as HrpcServerError, Socket, SocketError},
        warp::reply::Response,
        BodyKind, Request,
    },
    harmonytypes::{ItemPosition, Metadata},
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
    set_proto_name, ServerError,
};

use super::{profile::ProfileTree, Dependencies};

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

pub struct ChatServer {
    host: String,
    media_root: PathBuf,
    valid_sessions: auth::SessionMap,
    chat_tree: ChatTree,
    profile_tree: ProfileTree,
    pub broadcast_send: EventSender,
    dispatch_tx: EventDispatcher,
    disable_ratelimits: bool,
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
            disable_ratelimits: deps.config.disable_ratelimits,
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
                        if !matches!(err, SocketError::Closed) {
                            tracing::error!(
                                "couldnt write to stream events socket for user {}: {}",
                                user_id,
                                err
                            );
                        }
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

                                matches!(perm, Ok(_) | Err(ServerError::EmptyPermissionQuery))
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

    fn dispatch_guild_leave(&self, guild_id: u64, user_id: u64) {
        match self.profile_tree.local_to_foreign_id(user_id) {
            Some((foreign_id, target)) => self.dispatch_event(
                target,
                DispatchKind::UserRemovedFromGuild(SyncUserRemovedFromGuild {
                    user_id: foreign_id,
                    guild_id,
                }),
            ),
            None => {
                self.chat_tree
                    .remove_guild_from_guild_list(user_id, guild_id, "");
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
    }
}

#[harmony_rust_sdk::api::exports::hrpc::async_trait]
impl chat_service_server::ChatService for ChatServer {
    type Error = ServerError;

    #[rate(1, 5)]
    async fn create_guild(
        &self,
        request: Request<CreateGuildRequest>,
    ) -> Result<CreateGuildResponse, HrpcServerError<Self::Error>> {
        auth!();

        let (body, headers, addr) = request.into_parts();

        let CreateGuildRequest {
            metadata,
            name,
            picture,
        } = body.into_message().await??;

        let guild_id = {
            let mut rng = rand::thread_rng();
            let mut guild_id = rng.gen_range(1..u64::MAX);
            while self
                .chat_tree
                .chat_tree
                .contains_key(&guild_id.to_be_bytes())
                .unwrap()
            {
                guild_id = rng.gen_range(1..u64::MAX);
            }
            guild_id
        };

        let guild = Guild {
            name,
            picture,
            owner_id: user_id,
            metadata,
        };
        let buf = rkyv_ser(&guild);

        chat_insert!(guild_id.to_be_bytes() / buf);
        chat_insert!(make_member_key(guild_id, user_id) / []);

        // Some basic default setup
        let everyone_role_id = self.chat_tree.add_guild_role_logic(
            guild_id,
            Role {
                name: "everyone".to_string(),
                pingable: false,
                ..Default::default()
            },
        )?;
        // [tag:default_role_store]
        chat_insert!(make_guild_default_role_key(guild_id) / everyone_role_id.to_be_bytes());
        self.chat_tree.add_default_role_to(guild_id, user_id)?;
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
        self.chat_tree
            .set_permissions_logic(guild_id, None, everyone_role_id, def_perms.clone());
        let channel_id = self
            .create_channel(Request::from_parts((
                BodyKind::DecodedMessage(CreateChannelRequest {
                    guild_id,
                    channel_name: "general".to_string(),
                    ..Default::default()
                }),
                headers,
                addr,
            )))
            .await?
            .channel_id;
        self.chat_tree.set_permissions_logic(
            guild_id,
            Some(channel_id),
            everyone_role_id,
            def_perms,
        );

        match self.profile_tree.local_to_foreign_id(user_id) {
            Some((foreign_id, target)) => self.dispatch_event(
                target,
                DispatchKind::UserAddedToGuild(SyncUserAddedToGuild {
                    user_id: foreign_id,
                    guild_id,
                }),
            ),
            None => {
                self.chat_tree
                    .add_guild_to_guild_list(user_id, guild_id, "");
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

        Ok(CreateGuildResponse { guild_id })
    }

    #[rate(5, 5)]
    async fn create_invite(
        &self,
        request: Request<CreateInviteRequest>,
    ) -> Result<CreateInviteResponse, HrpcServerError<Self::Error>> {
        auth!();

        let CreateInviteRequest {
            guild_id,
            name,
            possible_uses,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "invites.manage.create", false)?;

        let key = make_invite_key(name.as_str());

        if name.is_empty() {
            return Err(ServerError::InviteNameEmpty.into());
        }

        if chat_get!(key).is_some() {
            return Err(ServerError::InviteExists(name).into());
        }

        let invite = Invite {
            possible_uses,
            use_count: 0,
        };
        let buf = rkyv_ser(&invite);

        chat_insert!(key / [guild_id.to_be_bytes().as_ref(), buf.as_ref()].concat());

        Ok(CreateInviteResponse { invite_id: name })
    }

    #[rate(5, 5)]
    async fn create_channel(
        &self,
        request: Request<CreateChannelRequest>,
    ) -> Result<CreateChannelResponse, HrpcServerError<Self::Error>> {
        auth!();

        let CreateChannelRequest {
            guild_id,
            channel_name,
            is_category,
            position,
            metadata,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "channels.manage.create", false)?;

        let channel_id = self.chat_tree.create_channel_logic(
            guild_id,
            channel_name.clone(),
            is_category,
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
                is_category,
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

        Ok(CreateChannelResponse { channel_id })
    }

    #[rate(15, 5)]
    async fn get_guild_list(
        &self,
        request: Request<GetGuildListRequest>,
    ) -> Result<GetGuildListResponse, HrpcServerError<Self::Error>> {
        auth!();

        let prefix = make_guild_list_key_prefix(user_id);
        let guilds = self
            .chat_tree
            .chat_tree
            .scan_prefix(&prefix)
            .map(|res| {
                let (guild_id_raw, _) = res.unwrap();
                let (id_raw, host_raw) = guild_id_raw
                    .split_at(prefix.len())
                    .1
                    .split_at(size_of::<u64>());

                // Safety: this unwrap can never cause UB since we split at u64 boundary
                let guild_id = u64::from_be_bytes(unsafe { id_raw.try_into().unwrap_unchecked() });
                // Safety: we never store non UTF-8 hosts, so this can't cause UB
                let host = unsafe { std::str::from_utf8_unchecked(host_raw) };

                GuildListEntry {
                    guild_id,
                    server_id: host.to_string(),
                }
            })
            .collect();

        Ok(GetGuildListResponse { guilds })
    }

    #[rate(15, 5)]
    async fn get_guild(
        &self,
        request: Request<GetGuildRequest>,
    ) -> Result<GetGuildResponse, HrpcServerError<Self::Error>> {
        auth!();

        let GetGuildRequest { guild_id } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        self.chat_tree
            .get_guild_logic(guild_id)
            .map(|g| GetGuildResponse { guild: Some(g) })
            .map_err(Into::into)
    }

    #[rate(15, 5)]
    async fn get_guild_invites(
        &self,
        request: Request<GetGuildInvitesRequest>,
    ) -> Result<GetGuildInvitesResponse, HrpcServerError<Self::Error>> {
        auth!();

        let GetGuildInvitesRequest { guild_id } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "invites.view", false)?;

        Ok(self.chat_tree.get_guild_invites_logic(guild_id))
    }

    #[rate(15, 5)]
    async fn get_guild_members(
        &self,
        request: Request<GetGuildMembersRequest>,
    ) -> Result<GetGuildMembersResponse, HrpcServerError<Self::Error>> {
        auth!();

        let GetGuildMembersRequest { guild_id } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        Ok(self.chat_tree.get_guild_members_logic(guild_id))
    }

    #[rate(15, 5)]
    async fn get_guild_channels(
        &self,
        request: Request<GetGuildChannelsRequest>,
    ) -> Result<GetGuildChannelsResponse, HrpcServerError<Self::Error>> {
        auth!();

        let GetGuildChannelsRequest { guild_id } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        Ok(self.chat_tree.get_guild_channels_logic(guild_id, user_id))
    }

    #[rate(10, 5)]
    async fn get_channel_messages(
        &self,
        request: Request<GetChannelMessagesRequest>,
    ) -> Result<GetChannelMessagesResponse, HrpcServerError<Self::Error>> {
        auth!();

        let GetChannelMessagesRequest {
            guild_id,
            channel_id,
            message_id,
            direction,
            count,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree
            .check_perms(guild_id, Some(channel_id), user_id, "messages.view", false)?;

        Ok(self.chat_tree.get_channel_messages_logic(
            guild_id,
            channel_id,
            message_id,
            direction.map(|val| Direction::from_i32(val).unwrap_or_default()),
            count,
        ))
    }

    #[rate(10, 5)]
    async fn get_message(
        &self,
        request: Request<GetMessageRequest>,
    ) -> Result<GetMessageResponse, HrpcServerError<Self::Error>> {
        auth!();

        let request = request.into_parts().0.into_message().await??;

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

        Ok(GetMessageResponse { message })
    }

    #[rate(2, 5)]
    async fn update_guild_information(
        &self,
        request: Request<UpdateGuildInformationRequest>,
    ) -> Result<UpdateGuildInformationResponse, HrpcServerError<Self::Error>> {
        auth!();

        let UpdateGuildInformationRequest {
            guild_id,
            new_guild_name,
            new_guild_picture,
            new_metadata,
        } = request.into_parts().0.into_message().await??;

        let key = guild_id.to_be_bytes();
        let mut guild_info = if let Some(raw) = self.chat_tree.chat_tree.get(&key).unwrap() {
            db::deser_guild(raw)
        } else {
            return Err(ServerError::NoSuchGuild(guild_id).into());
        };

        self.chat_tree.is_user_in_guild(guild_id, user_id)?;

        self.chat_tree.check_perms(
            guild_id,
            None,
            user_id,
            "guild.manage.change-information",
            false,
        )?;

        if let Some(new_guild_name) = new_guild_name.clone() {
            guild_info.name = new_guild_name;
        }
        if let Some(new_guild_picture) = new_guild_picture.clone() {
            guild_info.picture = new_guild_picture;
        }
        if let Some(new_metadata) = new_metadata.clone() {
            guild_info.metadata = Some(new_metadata);
        }

        let buf = rkyv_ser(&guild_info);
        chat_insert!(key / buf);

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::EditedGuild(stream_event::GuildUpdated {
                guild_id,
                new_name: new_guild_name,
                new_picture: new_guild_picture,
                new_metadata,
            }),
            None,
            EventContext::empty(),
        );

        Ok(UpdateGuildInformationResponse {})
    }

    #[rate(2, 5)]
    async fn update_channel_information(
        &self,
        request: Request<UpdateChannelInformationRequest>,
    ) -> Result<UpdateChannelInformationResponse, HrpcServerError<Self::Error>> {
        auth!();

        let UpdateChannelInformationRequest {
            guild_id,
            channel_id,
            new_name,
            new_metadata,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.check_perms(
            guild_id,
            Some(channel_id),
            user_id,
            "channels.manage.change-information",
            false,
        )?;

        let key = make_chan_key(guild_id, channel_id);
        let mut chan_info = if let Some(raw) = self.chat_tree.chat_tree.get(&key).unwrap() {
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
        chat_insert!(key / buf);

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

        Ok(UpdateChannelInformationResponse {})
    }

    #[rate(10, 5)]
    async fn update_channel_order(
        &self,
        request: Request<UpdateChannelOrderRequest>,
    ) -> Result<UpdateChannelOrderResponse, HrpcServerError<Self::Error>> {
        auth!();

        let UpdateChannelOrderRequest {
            guild_id,
            channel_id,
            new_position,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree.check_perms(
            guild_id,
            Some(channel_id),
            user_id,
            "channels.manage.move",
            false,
        )?;

        if let Some(position) = new_position.clone() {
            self.chat_tree
                .update_channel_order_logic(guild_id, channel_id, position)?
        }

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::EditedChannelPosition(stream_event::ChannelPositionUpdated {
                guild_id,
                channel_id,
                new_position,
            }),
            Some(PermCheck::new(
                guild_id,
                Some(channel_id),
                "messages.view",
                false,
            )),
            EventContext::empty(),
        );

        Ok(UpdateChannelOrderResponse {})
    }

    #[rate(1, 5)]
    async fn update_all_channel_order(
        &self,
        request: Request<UpdateAllChannelOrderRequest>,
    ) -> Result<UpdateAllChannelOrderResponse, HrpcServerError<Self::Error>> {
        auth!();

        let UpdateAllChannelOrderRequest {
            guild_id,
            channel_ids,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "channels.manage.move", false)?;

        for channel_id in &channel_ids {
            self.chat_tree.does_channel_exist(guild_id, *channel_id)?;
        }

        let prefix = make_guild_chan_prefix(guild_id);
        let channels = self
            .chat_tree
            .chat_tree
            .scan_prefix(&prefix)
            .flat_map(|res| {
                let (key, _) = res.unwrap();
                (key.len() == prefix.len() + size_of::<u64>())
                    .then(|| unsafe { u64::from_be_bytes(key.try_into().unwrap_unchecked()) })
            })
            .collect::<Vec<_>>();

        for channel_id in channels {
            if !channel_ids.contains(&channel_id) {
                return Err(ServerError::UnderSpecifiedChannels.into());
            }
        }

        let key = make_guild_chan_ordering_key(guild_id);
        let serialized_ordering = self.chat_tree.serialize_list_u64_logic(channel_ids.clone());
        chat_insert!(key / serialized_ordering);

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::ChannelsReordered(stream_event::ChannelsReordered {
                guild_id,
                channel_ids,
            }),
            None,
            EventContext::empty(),
        );

        Ok(UpdateAllChannelOrderResponse {})
    }

    #[rate(2, 5)]
    async fn update_message_text(
        &self,
        request: Request<UpdateMessageTextRequest>,
    ) -> Result<UpdateMessageTextResponse, HrpcServerError<Self::Error>> {
        auth!();

        let request = request.into_parts().0.into_message().await??;

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
        chat_insert!(key / buf);

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

        Ok(UpdateMessageTextResponse {})
    }

    #[rate(1, 15)]
    async fn delete_guild(
        &self,
        request: Request<DeleteGuildRequest>,
    ) -> Result<DeleteGuildResponse, HrpcServerError<Self::Error>> {
        auth!();

        let DeleteGuildRequest { guild_id } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "guild.manage.delete", false)?;

        let guild_members = self.chat_tree.get_guild_members_logic(guild_id).members;

        let guild_data = self
            .chat_tree
            .chat_tree
            .scan_prefix(&guild_id.to_be_bytes())
            .map(|res| res.unwrap().0);

        let mut batch = Batch::default();
        for key in guild_data {
            batch.remove(key);
        }
        self.chat_tree.chat_tree.apply_batch(batch).unwrap();

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::DeletedGuild(stream_event::GuildDeleted { guild_id }),
            None,
            EventContext::empty(),
        );

        let mut local_ids = Vec::new();
        for member_id in guild_members {
            match self.profile_tree.local_to_foreign_id(member_id) {
                Some((foreign_id, target)) => self.dispatch_event(
                    target,
                    DispatchKind::UserRemovedFromGuild(SyncUserRemovedFromGuild {
                        user_id: foreign_id,
                        guild_id,
                    }),
                ),
                None => {
                    self.chat_tree
                        .remove_guild_from_guild_list(user_id, guild_id, "");
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

        Ok(DeleteGuildResponse {})
    }

    #[rate(5, 5)]
    async fn delete_invite(
        &self,
        request: Request<DeleteInviteRequest>,
    ) -> Result<DeleteInviteResponse, HrpcServerError<Self::Error>> {
        auth!();

        let DeleteInviteRequest {
            guild_id,
            invite_id,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "invites.manage.delete", false)?;

        self.chat_tree
            .chat_tree
            .remove(&make_invite_key(invite_id.as_str()))
            .unwrap();

        Ok(DeleteInviteResponse {})
    }

    #[rate(5, 5)]
    async fn delete_channel(
        &self,
        request: Request<DeleteChannelRequest>,
    ) -> Result<DeleteChannelResponse, HrpcServerError<Self::Error>> {
        auth!();

        let DeleteChannelRequest {
            guild_id,
            channel_id,
        } = request.into_parts().0.into_message().await??;

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
            .map(|res| res.unwrap().0);

        // Remove from ordering list
        let key = make_guild_chan_ordering_key(guild_id);
        let mut ordering = self.chat_tree.get_list_u64_logic(&key);
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
        self.chat_tree.chat_tree.apply_batch(batch).unwrap();

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::DeletedChannel(stream_event::ChannelDeleted {
                guild_id,
                channel_id,
            }),
            None,
            EventContext::empty(),
        );

        Ok(DeleteChannelResponse {})
    }

    #[rate(5, 5)]
    async fn delete_message(
        &self,
        request: Request<DeleteMessageRequest>,
    ) -> Result<DeleteMessageResponse, HrpcServerError<Self::Error>> {
        auth!();

        let DeleteMessageRequest {
            guild_id,
            channel_id,
            message_id,
        } = request.into_parts().0.into_message().await??;

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
            .unwrap();

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

        Ok(DeleteMessageResponse {})
    }

    #[rate(5, 5)]
    async fn join_guild(
        &self,
        request: Request<JoinGuildRequest>,
    ) -> Result<JoinGuildResponse, HrpcServerError<Self::Error>> {
        auth!();

        let JoinGuildRequest { invite_id } = request.into_parts().0.into_message().await??;
        let key = make_invite_key(invite_id.as_str());

        let (guild_id, mut invite) = if let Some(raw) = self.chat_tree.chat_tree.get(&key).unwrap()
        {
            db::deser_invite_entry(raw)
        } else {
            return Err(ServerError::NoSuchInvite(invite_id.into()).into());
        };

        if self.chat_tree.is_user_banned_in_guild(guild_id, user_id) {
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

        chat_insert!(make_member_key(guild_id, user_id) / []);
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

        match self.profile_tree.local_to_foreign_id(user_id) {
            Some((foreign_id, target)) => self.dispatch_event(
                target,
                DispatchKind::UserAddedToGuild(SyncUserAddedToGuild {
                    user_id: foreign_id,
                    guild_id,
                }),
            ),
            None => {
                self.chat_tree
                    .add_guild_to_guild_list(user_id, guild_id, "");
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

        let buf = rkyv_ser(&invite);
        chat_insert!(key / [guild_id.to_be_bytes().as_ref(), buf.as_ref()].concat());

        Ok(JoinGuildResponse { guild_id })
    }

    #[rate(5, 5)]
    async fn leave_guild(
        &self,
        request: Request<LeaveGuildRequest>,
    ) -> Result<LeaveGuildResponse, HrpcServerError<Self::Error>> {
        auth!();

        let LeaveGuildRequest { guild_id } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        self.chat_tree
            .chat_tree
            .remove(&make_member_key(guild_id, user_id))
            .unwrap();

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

        self.dispatch_guild_leave(guild_id, user_id);

        Ok(LeaveGuildResponse {})
    }

    #[rate(20, 5)]
    async fn trigger_action(
        &self,
        _request: Request<TriggerActionRequest>,
    ) -> Result<TriggerActionResponse, HrpcServerError<Self::Error>> {
        Err(ServerError::NotImplemented.into())
    }

    #[rate(50, 10)]
    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<SendMessageResponse, HrpcServerError<Self::Error>> {
        auth!();

        let SendMessageRequest {
            guild_id,
            channel_id,
            content,
            in_reply_to,
            overrides,
            echo_id,
            metadata,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree
            .check_perms(guild_id, Some(channel_id), user_id, "messages.send", false)?;

        let msg_prefix = make_msg_prefix(guild_id, channel_id);
        let message_id = self
            .chat_tree
            .chat_tree
            .scan_prefix(&msg_prefix)
            .last()
            // Ensure that the first message ID is always 1!
            .map_or(1, |res| {
                // Safety: this won't cause UB since we only store u64 after the prefix [ref:msg_key_u64]
                u64::from_be_bytes(unsafe {
                    res.unwrap()
                        .0
                        .split_at(msg_prefix.len())
                        .1
                        .try_into()
                        .unwrap_unchecked()
                }) + 1
            });
        let key = make_msg_key(guild_id, channel_id, message_id); // [tag:msg_key_u64]

        let created_at = get_time_secs();
        let edited_at = None;

        let inner_content = content.and_then(|c| c.content);
        let content = if let Some(content) = inner_content {
            let content = match content {
                content::Content::TextMessage(text) => {
                    if text.content.as_ref().map_or(true, |f| f.text.is_empty()) {
                        return Err(ServerError::MessageContentCantBeEmpty.into());
                    }
                    content::Content::TextMessage(text)
                }
                content::Content::PhotoMessage(mut photos) => {
                    if photos.photos.is_empty() {
                        return Err(ServerError::MessageContentCantBeEmpty.into());
                    }
                    for photo in photos.photos.drain(..).collect::<Vec<_>>() {
                        // TODO: return error for invalid hmc
                        if let Ok(hmc) = Hmc::from_str(&photo.hmc) {
                            const FORMAT: image::ImageFormat = image::ImageFormat::Jpeg;

                            // TODO: check if the hmc host matches ours, if not fetch the image from the other host
                            let id = hmc.id();

                            let image_jpeg_id = format!("{}_{}", id, "jpeg");
                            let image_jpeg_path = self.media_root.join(&image_jpeg_id);
                            let minithumbnail_jpeg_id = format!("{}_{}", id, "jpegthumb");
                            let minithumbnail_jpeg_path =
                                self.media_root.join(&minithumbnail_jpeg_id);

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
                                let (_, _, data, _) =
                                    get_file_full(self.media_root.as_path(), id).await?;

                                if image::guess_format(&data).is_err() {
                                    return Err(ServerError::NotAnImage.into());
                                }

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
                                hmc: Hmc::new(&self.host, image_jpeg_id).unwrap().into(),
                                minithumbnail: Some(minithumbnail),
                                width: isize.0,
                                height: isize.1,
                                file_size: file_size as u32,
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
                        return Err(ServerError::MessageContentCantBeEmpty.into());
                    }
                    for attachment in files.files.drain(..).collect::<Vec<_>>() {
                        if let Ok(id) = FileId::from_str(&attachment.id) {
                            let media_root = self.media_root.as_path();
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
                        return Err(ServerError::MessageContentCantBeEmpty.into());
                    }
                    content::Content::EmbedMessage(embed)
                }
            };
            Some(Content {
                content: Some(content),
            })
        } else {
            return Err(ServerError::MessageContentCantBeEmpty.into());
        };

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
        chat_insert!(key / value);

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

        Ok(SendMessageResponse { message_id })
    }

    #[rate(30, 5)]
    async fn query_has_permission(
        &self,
        request: Request<QueryHasPermissionRequest>,
    ) -> Result<QueryHasPermissionResponse, HrpcServerError<Self::Error>> {
        auth!();

        self.chat_tree
            .query_has_permission_request(user_id, request.into_parts().0.into_message().await??)
            .map_err(Into::into)
    }

    #[rate(10, 5)]
    async fn set_permissions(
        &self,
        request: Request<SetPermissionsRequest>,
    ) -> Result<SetPermissionsResponse, HrpcServerError<Self::Error>> {
        auth!();

        let SetPermissionsRequest {
            guild_id,
            channel_id,
            role_id,
            perms_to_give,
        } = request.into_parts().0.into_message().await??;

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
            );
            let members = self.chat_tree.get_guild_members_logic(guild_id).members;
            let guild_owner = self.chat_tree.get_guild_owner(guild_id)?;
            let for_users = members
                .iter()
                .filter_map(|user_id| {
                    guild_owner
                        .ne(user_id)
                        .then(|| {
                            self.chat_tree
                                .get_user_roles_logic(guild_id, *user_id)
                                .contains(&role_id)
                                .then(|| *user_id)
                        })
                        .flatten()
                })
                .collect::<Vec<_>>();
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
            Ok(SetPermissionsResponse {})
        } else {
            Err(ServerError::NoPermissionsSpecified.into())
        }
    }

    #[rate(10, 5)]
    async fn get_permissions(
        &self,
        request: Request<GetPermissionsRequest>,
    ) -> Result<GetPermissionsResponse, HrpcServerError<Self::Error>> {
        auth!();

        let GetPermissionsRequest {
            guild_id,
            channel_id,
            role_id,
        } = request.into_parts().0.into_message().await??;

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
            .get_permissions_logic(guild_id, channel_id, role_id)
            .into_iter()
            .map(|(m, ok)| Permission {
                matches: m.into(),
                ok,
            })
            .collect();

        Ok(GetPermissionsResponse { perms })
    }

    #[rate(10, 5)]
    async fn move_role(
        &self,
        request: Request<MoveRoleRequest>,
    ) -> Result<MoveRoleResponse, HrpcServerError<Self::Error>> {
        auth!();

        let MoveRoleRequest {
            guild_id,
            role_id,
            new_position,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "roles.manage", false)?;
        self.chat_tree.does_role_exist(guild_id, role_id)?;

        if let Some(pos) = new_position.clone() {
            self.chat_tree.move_role_logic(guild_id, role_id, pos)?;
        }

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::RoleMoved(stream_event::RoleMoved {
                guild_id,
                role_id,
                new_position,
            }),
            None,
            EventContext::empty(),
        );

        Ok(MoveRoleResponse {})
    }

    #[rate(10, 5)]
    async fn get_guild_roles(
        &self,
        request: Request<GetGuildRolesRequest>,
    ) -> Result<GetGuildRolesResponse, HrpcServerError<Self::Error>> {
        auth!();

        let GetGuildRolesRequest { guild_id } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "roles.get", false)?;

        let roles = self.chat_tree.get_guild_roles_logic(guild_id);

        Ok(GetGuildRolesResponse { roles })
    }

    #[rate(10, 5)]
    async fn add_guild_role(
        &self,
        request: Request<AddGuildRoleRequest>,
    ) -> Result<AddGuildRoleResponse, HrpcServerError<Self::Error>> {
        auth!();

        let AddGuildRoleRequest {
            guild_id,
            name,
            color,
            hoist,
            pingable,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "roles.manage", false)?;

        let role = Role {
            name: name.clone(),
            color,
            hoist,
            pingable,
        };
        let role_id = self.chat_tree.add_guild_role_logic(guild_id, role)?;
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

        Ok(AddGuildRoleResponse { role_id })
    }

    #[rate(10, 5)]
    async fn modify_guild_role(
        &self,
        request: Request<ModifyGuildRoleRequest>,
    ) -> Result<ModifyGuildRoleResponse, HrpcServerError<Self::Error>> {
        auth!();

        let ModifyGuildRoleRequest {
            guild_id,
            role_id,
            new_name,
            new_color,
            new_hoist,
            new_pingable,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "roles.manage", false)?;

        let key = make_guild_role_key(guild_id, role_id);
        let mut role = if let Some(raw) = self.chat_tree.chat_tree.get(&key).unwrap() {
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
        chat_insert!(key / ser_role);

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

        Ok(ModifyGuildRoleResponse {})
    }

    #[rate(10, 5)]
    async fn delete_guild_role(
        &self,
        request: Request<DeleteGuildRoleRequest>,
    ) -> Result<DeleteGuildRoleResponse, HrpcServerError<Self::Error>> {
        auth!();

        let DeleteGuildRoleRequest { guild_id, role_id } =
            request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "roles.manage", false)?;

        self.chat_tree
            .chat_tree
            .remove(&make_guild_role_key(guild_id, role_id))
            .unwrap()
            .ok_or(ServerError::NoSuchRole { guild_id, role_id })?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            stream_event::Event::RoleDeleted(stream_event::RoleDeleted { guild_id, role_id }),
            None,
            EventContext::empty(),
        );

        Ok(DeleteGuildRoleResponse {})
    }

    #[rate(10, 5)]
    async fn manage_user_roles(
        &self,
        request: Request<ManageUserRolesRequest>,
    ) -> Result<ManageUserRolesResponse, HrpcServerError<Self::Error>> {
        auth!();

        let ManageUserRolesRequest {
            guild_id,
            user_id: user_to_manage,
            give_role_ids,
            take_role_ids,
        } = request.into_parts().0.into_message().await??;

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

        Ok(ManageUserRolesResponse {})
    }

    #[rate(10, 5)]
    async fn get_user_roles(
        &self,
        request: Request<GetUserRolesRequest>,
    ) -> Result<GetUserRolesResponse, HrpcServerError<Self::Error>> {
        auth!();

        let GetUserRolesRequest {
            guild_id,
            user_id: user_to_fetch,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.is_user_in_guild(guild_id, user_to_fetch)?;
        let fetch_user = (user_to_fetch == 0)
            .then(|| user_id)
            .unwrap_or(user_to_fetch);
        if fetch_user != user_id {
            self.chat_tree
                .check_perms(guild_id, None, user_id, "roles.user.get", false)?;
        }

        let roles = self.chat_tree.get_user_roles_logic(guild_id, fetch_user);

        Ok(GetUserRolesResponse { roles })
    }

    type StreamEventsValidationType = u64;

    #[rate(4, 5)]
    async fn typing(
        &self,
        request: Request<TypingRequest>,
    ) -> Result<TypingResponse, HrpcServerError<Self::Error>> {
        auth!();

        let TypingRequest {
            guild_id,
            channel_id,
        } = request.into_parts().0.into_message().await??;

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

        Ok(TypingResponse {})
    }

    #[rate(2, 5)]
    async fn preview_guild(
        &self,
        request: Request<PreviewGuildRequest>,
    ) -> Result<PreviewGuildResponse, HrpcServerError<Self::Error>> {
        let PreviewGuildRequest { invite_id } = request.into_parts().0.into_message().await??;

        let key = make_invite_key(&invite_id);
        let guild_id = self
            .chat_tree
            .chat_tree
            .get(&key)
            .unwrap()
            .ok_or_else(|| ServerError::NoSuchInvite(invite_id.into()))
            .map(|raw| db::deser_invite_entry_guild_id(&raw))?;
        let guild = self.chat_tree.get_guild_logic(guild_id)?;
        let member_count = self
            .chat_tree
            .chat_tree
            .scan_prefix(&make_guild_mem_prefix(guild_id))
            .count();

        Ok(PreviewGuildResponse {
            name: guild.name,
            picture: guild.picture,
            member_count: member_count as u64,
        })
    }

    async fn get_banned_users(
        &self,
        _request: Request<GetBannedUsersRequest>,
    ) -> Result<GetBannedUsersResponse, HrpcServerError<Self::Error>> {
        Err(ServerError::NotImplemented.into())
    }

    #[rate(4, 5)]
    async fn ban_user(
        &self,
        request: Request<BanUserRequest>,
    ) -> Result<BanUserResponse, HrpcServerError<Self::Error>> {
        auth!();

        let BanUserRequest {
            guild_id,
            user_id: user_to_ban,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.is_user_in_guild(guild_id, user_to_ban)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "user.manage.ban", false)?;

        self.chat_tree.kick_user_logic(guild_id, user_to_ban);

        chat_insert!(make_banned_member_key(guild_id, user_to_ban) / get_time_secs().to_be_bytes());

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

        self.dispatch_guild_leave(guild_id, user_to_ban);

        Ok(BanUserResponse {})
    }

    #[rate(4, 5)]
    async fn kick_user(
        &self,
        request: Request<KickUserRequest>,
    ) -> Result<KickUserResponse, HrpcServerError<Self::Error>> {
        auth!();

        let KickUserRequest {
            guild_id,
            user_id: user_to_kick,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.is_user_in_guild(guild_id, user_to_kick)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "user.manage.kick", false)?;

        self.chat_tree.kick_user_logic(guild_id, user_to_kick);

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

        self.dispatch_guild_leave(guild_id, user_to_kick);

        Ok(KickUserResponse {})
    }

    #[rate(4, 5)]
    async fn unban_user(
        &self,
        request: Request<UnbanUserRequest>,
    ) -> Result<UnbanUserResponse, HrpcServerError<Self::Error>> {
        auth!();

        let UnbanUserRequest {
            guild_id,
            user_id: user_to_unban,
        } = request.into_parts().0.into_message().await??;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, None, user_id, "user.manage.unban", false)?;

        chat_remove!(make_banned_member_key(guild_id, user_to_unban));

        Ok(UnbanUserResponse {})
    }

    async fn get_pinned_messages(
        &self,
        _request: Request<GetPinnedMessagesRequest>,
    ) -> Result<GetPinnedMessagesResponse, HrpcServerError<Self::Error>> {
        Err(ServerError::NotImplemented.into())
    }

    async fn pin_message(
        &self,
        _request: Request<PinMessageRequest>,
    ) -> Result<PinMessageResponse, HrpcServerError<Self::Error>> {
        Err(ServerError::NotImplemented.into())
    }

    async fn unpin_message(
        &self,
        _request: Request<UnpinMessageRequest>,
    ) -> Result<UnpinMessageResponse, HrpcServerError<Self::Error>> {
        Err(ServerError::NotImplemented.into())
    }

    fn stream_events_on_upgrade(&self, response: Response) -> Response {
        set_proto_name(response)
    }

    async fn stream_events_validation(
        &self,
        request: Request<Option<StreamEventsRequest>>,
    ) -> Result<u64, HrpcServerError<Self::Error>> {
        auth!();
        tracing::debug!("stream events validated for user {}", user_id);
        Ok(user_id)
    }

    #[rate(1, 5)]
    async fn stream_events(
        &self,
        user_id: u64,
        socket: Socket<StreamEventsRequest, StreamEventsResponse>,
    ) {
        tracing::debug!("creating stream events for user {}", user_id);
        let (sub_tx, sub_rx) = mpsc::channel(64);
        let chat_tree = self.chat_tree.clone();

        let send_loop = self.spawn_event_stream_processor(user_id, sub_rx, socket.clone());
        let recv_loop = async move {
            loop {
                return_print!(socket.receive_message().await, |req| {
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
                            Request::SubscribeToHomeserverEvents(
                                SubscribeToHomeserverEvents {},
                            ) => EventSub::Homeserver,
                        };

                        drop(sub_tx.send(sub).await);
                    }
                });
            }
        };

        tokio::select!(
            res = send_loop => {
                res.unwrap();
            }
            res = tokio::spawn(recv_loop) => {
                res.unwrap();
            }
        );
        tracing::debug!("stream events ended for user {}", user_id);
    }

    async fn add_reaction(
        &self,
        _request: Request<AddReactionRequest>,
    ) -> Result<AddReactionResponse, HrpcServerError<Self::Error>> {
        Err(ServerError::NotImplemented.into())
    }

    async fn remove_reaction(
        &self,
        _request: Request<RemoveReactionRequest>,
    ) -> Result<RemoveReactionResponse, HrpcServerError<Self::Error>> {
        Err(ServerError::NotImplemented.into())
    }
}

#[derive(Clone)]
pub struct ChatTree {
    pub chat_tree: db::ArcTree,
}

impl ChatTree {
    pub fn new(db: &dyn Db) -> DbResult<Self> {
        let chat_tree = db.open_tree(b"chat")?;
        Ok(Self { chat_tree })
    }

    pub fn is_user_in_guild(&self, guild_id: u64, user_id: u64) -> Result<(), ServerError> {
        self.chat_tree
            .contains_key(&make_member_key(guild_id, user_id))
            .unwrap()
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::UserNotInGuild { guild_id, user_id }))
    }

    pub fn does_guild_exist(&self, guild_id: u64) -> Result<(), ServerError> {
        self.chat_tree
            .contains_key(&guild_id.to_be_bytes())
            .unwrap()
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::NoSuchGuild(guild_id)))
    }

    pub fn does_channel_exist(&self, guild_id: u64, channel_id: u64) -> Result<(), ServerError> {
        self.chat_tree
            .contains_key(&make_chan_key(guild_id, channel_id))
            .unwrap()
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::NoSuchChannel {
                guild_id,
                channel_id,
            }))
    }

    pub fn does_role_exist(&self, guild_id: u64, role_id: u64) -> Result<(), ServerError> {
        self.chat_tree
            .contains_key(&make_guild_role_key(guild_id, role_id))
            .unwrap()
            .then(|| Ok(()))
            .unwrap_or(Err(ServerError::NoSuchRole { guild_id, role_id }))
    }

    pub fn is_user_banned_in_guild(&self, guild_id: u64, user_id: u64) -> bool {
        self.chat_tree
            .contains_key(&make_banned_member_key(guild_id, user_id))
            .unwrap()
    }

    pub fn get_guild_owner(&self, guild_id: u64) -> Result<u64, ServerError> {
        self.chat_tree
            .get(guild_id.to_be_bytes().as_ref())
            .unwrap()
            .map_or_else(
                || Err(ServerError::NoSuchGuild(guild_id)),
                |raw| Ok(db::deser_guild(raw).owner_id),
            )
    }

    pub fn is_user_guild_owner(&self, guild_id: u64, user_id: u64) -> Result<bool, ServerError> {
        self.get_guild_owner(guild_id).map(|owner| owner == user_id)
    }

    pub fn check_guild_user_channel(
        &self,
        guild_id: u64,
        user_id: u64,
        channel_id: u64,
    ) -> Result<(), ServerError> {
        self.check_guild_user(guild_id, user_id)?;
        self.does_channel_exist(guild_id, channel_id)
    }

    pub fn check_guild_user(&self, guild_id: u64, user_id: u64) -> Result<(), ServerError> {
        self.check_guild(guild_id)?;
        self.is_user_in_guild(guild_id, user_id)
    }

    #[inline(always)]
    pub fn check_guild(&self, guild_id: u64) -> Result<(), ServerError> {
        self.does_guild_exist(guild_id)
    }

    pub fn get_message_logic(
        &self,
        guild_id: u64,
        channel_id: u64,
        message_id: u64,
    ) -> Result<(HarmonyMessage, [u8; 26]), ServerError> {
        let key = make_msg_key(guild_id, channel_id, message_id);

        let message = if let Some(msg) = self.chat_tree.get(&key).unwrap() {
            db::deser_message(msg)
        } else {
            return Err(ServerError::NoSuchMessage {
                guild_id,
                channel_id,
                message_id,
            });
        };

        Ok((message, key))
    }

    pub fn get_guild_logic(&self, guild_id: u64) -> Result<Guild, ServerError> {
        let guild =
            if let Some(guild_raw) = self.chat_tree.get(guild_id.to_be_bytes().as_ref()).unwrap() {
                db::deser_guild(guild_raw)
            } else {
                return Err(ServerError::NoSuchGuild(guild_id));
            };

        Ok(guild)
    }

    pub fn get_guild_invites_logic(&self, guild_id: u64) -> GetGuildInvitesResponse {
        let invites = self
            .chat_tree
            .scan_prefix(INVITE_PREFIX)
            .map(|res| {
                let (key, value) = res.unwrap();
                let (inv_guild_id_raw, invite_raw) = value.split_at(size_of::<u64>());
                // Safety: this unwrap cannot fail since we split at u64 boundary
                let inv_guild_id =
                    u64::from_be_bytes(unsafe { inv_guild_id_raw.try_into().unwrap_unchecked() });
                let invite_id =
                    unsafe { std::str::from_utf8_unchecked(key.split_at(INVITE_PREFIX.len()).1) };
                let invite = db::deser_invite(invite_raw.into());
                (inv_guild_id, invite_id.to_string(), invite)
            })
            .filter_map(|(inv_guild_id, invite_id, invite)| {
                if guild_id == inv_guild_id {
                    Some(InviteWithId {
                        invite_id,
                        invite: Some(invite),
                    })
                } else {
                    None
                }
            })
            .collect();

        GetGuildInvitesResponse { invites }
    }

    pub fn get_guild_members_logic(&self, guild_id: u64) -> GetGuildMembersResponse {
        let prefix = make_guild_mem_prefix(guild_id);
        let members = self
            .chat_tree
            .scan_prefix(&prefix)
            .map(|res| {
                let (id, _) = res.unwrap();
                // Safety: this unwrap cannot fail since after we split at prefix length, the remainder is a valid u64
                u64::from_be_bytes(unsafe {
                    id.split_at(prefix.len()).1.try_into().unwrap_unchecked()
                })
            })
            .collect();

        GetGuildMembersResponse { members }
    }

    pub fn get_guild_channels_logic(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> GetGuildChannelsResponse {
        let prefix = make_guild_chan_prefix(guild_id);
        let mut channels = self
            .chat_tree
            .scan_prefix(&prefix)
            .flat_map(|res| {
                let (key, value) = res.unwrap();
                (key.len() == prefix.len() + size_of::<u64>())
                    .then(|| {
                        let channel_id = u64::from_be_bytes(
                            // Safety: this unwrap is safe since we check if it's a valid u64 beforehand
                            unsafe { key.split_at(prefix.len()).1.try_into().unwrap_unchecked() },
                        );
                        self.check_perms(
                            guild_id,
                            Some(channel_id),
                            user_id,
                            "messages.view",
                            false,
                        )
                        .is_ok()
                        // Safety: this unwrap is safe since we only store valid Channel message
                        .then(|| {
                            let channel = db::deser_chan(value);
                            ChannelWithId {
                                channel_id,
                                channel: Some(channel),
                            }
                        })
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();

        if channels.is_empty() {
            return GetGuildChannelsResponse {
                channels: Vec::new(),
            };
        }

        let ordering_raw = self
            .chat_tree
            .get(&make_guild_chan_ordering_key(guild_id))
            .unwrap()
            .unwrap_or_default();
        for (order_index, order_id) in db::make_u64_iter_logic(ordering_raw.as_ref()).enumerate() {
            if let Some(index) = channels.iter().position(|chan| chan.channel_id == order_id) {
                channels.swap(order_index, index);
            }
        }

        GetGuildChannelsResponse { channels }
    }

    #[inline(always)]
    pub fn get_list_u64_logic(&self, key: &[u8]) -> Vec<u64> {
        db::make_u64_iter_logic(
            self.chat_tree
                .get(key)
                .unwrap()
                .unwrap_or_default()
                .as_ref(),
        )
        .collect()
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
        position: impl Into<Place>,
    ) -> Result<(), ServerError> {
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
        position: impl Into<Place>,
    ) -> Result<(), ServerError> {
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
        position: impl Into<Place>,
        check_exists: impl Fn(u64) -> Result<(), ServerError>,
        key: &[u8],
    ) -> Result<(), ServerError> {
        let ser_ord_put = |ordering| {
            let serialized_ordering = self.serialize_list_u64_logic(ordering);
            cchat_insert!(key / serialized_ordering);
        };

        let place = position.into();
        let previous_id = place.after().unwrap_or(0);
        let next_id = place.before().unwrap_or(0);

        if previous_id != 0 {
            check_exists(previous_id)?;
        } else if next_id != 0 {
            check_exists(next_id)?;
        } else {
            let mut ordering = self.get_list_u64_logic(key);
            ordering.push(id);
            ser_ord_put(ordering);
            return Ok(());
        }

        let mut ordering = self.get_list_u64_logic(key);

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

        if let Some(index) = maybe_ord_index(previous_id) {
            maybe_replace_with(&mut ordering, index.saturating_add(1));
        } else if let Some(index) = maybe_ord_index(next_id) {
            maybe_replace_with(&mut ordering, index);
        }

        ser_ord_put(ordering);

        Ok(())
    }

    pub fn get_channel_messages_logic(
        &self,
        guild_id: u64,
        channel_id: u64,
        message_id: u64,
        direction: Option<Direction>,
        count: Option<u32>,
    ) -> GetChannelMessagesResponse {
        let direction = direction.unwrap_or_default();

        let prefix = make_msg_prefix(guild_id, channel_id);
        let get_last_message_id = || {
            self.chat_tree.scan_prefix(&prefix).last().map(|last| {
                u64::from_be_bytes(
                    // Safety: cannot fail since the remainder after we split is a valid u64
                    unsafe {
                        last.unwrap()
                            .0
                            .split_at(prefix.len())
                            .1
                            .try_into()
                            .unwrap_unchecked()
                    },
                )
            })
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
            let last_message_id = get_last_message_id().unwrap_or(1);

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
                .map(|res| {
                    let (key, value) = res.unwrap();
                    // Safety: this is safe since the only keys we get are message keys, which after stripping prefix are message IDs
                    let message_id = u64::from_be_bytes(unsafe {
                        key.split_at(make_msg_prefix(guild_id, channel_id).len())
                            .1
                            .try_into()
                            .unwrap_unchecked()
                    });
                    let message = db::deser_message(value);
                    MessageWithId {
                        message_id,
                        message: Some(message),
                    }
                })
                .collect();

            GetChannelMessagesResponse {
                reached_top: from == 1,
                reached_bottom: to == last_message_id,
                messages,
            }
        };

        (message_id == 0)
            .then(|| {
                get_last_message_id().map_or_else(
                    || GetChannelMessagesResponse {
                        reached_top: true,
                        reached_bottom: true,
                        messages: Vec::new(),
                    },
                    |last| get_messages(last + 1),
                )
            })
            .unwrap_or_else(|| get_messages(message_id))
    }

    pub fn get_user_roles_logic(&self, guild_id: u64, user_id: u64) -> Vec<u64> {
        let key = make_guild_user_roles_key(guild_id, user_id);
        self.chat_tree
            .get(&key)
            .unwrap()
            .map_or_else(Vec::default, |raw| {
                raw.chunks_exact(size_of::<u64>())
                    // Safety: this is safe since we split at u64 boundary
                    .map(|raw| u64::from_be_bytes(unsafe { raw.try_into().unwrap_unchecked() }))
                    .collect()
            })
    }

    pub fn query_has_permission_logic(
        &self,
        guild_id: u64,
        channel_id: Option<u64>,
        user_id: u64,
        check_for: &str,
    ) -> bool {
        let key = make_guild_user_roles_key(guild_id, user_id);
        let user_roles = self.get_list_u64_logic(&key);

        if let Some(channel_id) = channel_id {
            for role_id in &user_roles {
                let perms = self.get_permissions_logic(guild_id, Some(channel_id), *role_id);
                let is_allowed =
                    has_permission(perms.iter().map(|(m, ok)| (m.as_str(), *ok)), check_for);
                if let Some(true) = is_allowed {
                    return true;
                }
            }
        }

        for role_id in user_roles {
            let perms = self.get_permissions_logic(guild_id, None, role_id);
            let is_allowed =
                has_permission(perms.iter().map(|(m, ok)| (m.as_str(), *ok)), check_for);
            if let Some(true) = is_allowed {
                return true;
            }
        }

        false
    }

    pub fn check_perms(
        &self,
        guild_id: u64,
        channel_id: Option<u64>,
        user_id: u64,
        check_for: &str,
        must_be_guild_owner: bool,
    ) -> Result<(), ServerError> {
        let is_owner = self.is_user_guild_owner(guild_id, user_id)?;
        if must_be_guild_owner {
            if is_owner {
                return Ok(());
            }
        } else if is_owner
            || self.query_has_permission_logic(guild_id, channel_id, user_id, check_for)
        {
            return Ok(());
        }
        Err(ServerError::NotEnoughPermissions {
            must_be_guild_owner,
            missing_permission: check_for.into(),
        })
    }

    pub fn kick_user_logic(&self, guild_id: u64, user_id: u64) {
        self.chat_tree
            .remove(&make_member_key(guild_id, user_id))
            .unwrap();
        self.chat_tree
            .remove(&make_guild_user_roles_key(guild_id, user_id))
            .unwrap();
    }

    pub fn manage_user_roles_logic(
        &self,
        guild_id: u64,
        user_id: u64,
        give_role_ids: Vec<u64>,
        take_role_ids: Vec<u64>,
    ) -> Result<Vec<u64>, ServerError> {
        let mut roles = self.get_user_roles_logic(guild_id, user_id);
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
        cchat_insert!(key / ser_roles);

        Ok(roles)
    }

    pub fn add_default_role_to(&self, guild_id: u64, user_id: u64) -> Result<(), ServerError> {
        if let Some(raw) = self
            .chat_tree
            .get(&make_guild_default_role_key(guild_id))
            .unwrap()
        {
            // Safety: safe since we only store valid u64 [ref:default_role_store]
            let default_role_id = u64::from_be_bytes(unsafe { raw.try_into().unwrap_unchecked() });
            self.manage_user_roles_logic(guild_id, user_id, vec![default_role_id], Vec::new())?;
        }
        Ok(())
    }

    pub fn add_guild_role_logic(&self, guild_id: u64, role: Role) -> Result<u64, ServerError> {
        let (role_id, key) = {
            let mut rng = rand::thread_rng();
            let mut role_id = rng.gen_range(1..u64::MAX);
            let mut key = make_guild_role_key(guild_id, role_id);
            while self.chat_tree.contains_key(&key).unwrap() {
                role_id = rng.gen_range(1..u64::MAX);
                key = make_guild_role_key(guild_id, role_id);
            }
            (role_id, key)
        };
        let ser_role = rkyv_ser(&role);
        cchat_insert!(key / ser_role);
        self.move_role_logic(guild_id, role_id, Place::between(0, 0))?;
        Ok(role_id)
    }

    pub fn set_permissions_logic(
        &self,
        guild_id: u64,
        channel_id: Option<u64>,
        role_id: u64,
        perms_to_give: Vec<Permission>,
    ) {
        let mut batch = Batch::default();
        for perm in perms_to_give {
            let value = perm.ok.then(|| [1]).unwrap_or([0]);
            let key = channel_id.map_or_else(
                || make_guild_perm_key(guild_id, role_id, &perm.matches),
                |channel_id| make_channel_perm_key(guild_id, channel_id, role_id, &perm.matches),
            );
            batch.insert(key, value);
        }
        self.chat_tree.apply_batch(batch).unwrap();
    }

    pub fn create_channel_logic(
        &self,
        guild_id: u64,
        channel_name: String,
        is_category: bool,
        metadata: Option<Metadata>,
        position: Option<ItemPosition>,
    ) -> Result<u64, ServerError> {
        let channel_id = {
            let mut rng = rand::thread_rng();
            let mut channel_id = rng.gen_range(1..=u64::MAX);
            let mut key = make_chan_key(guild_id, channel_id);
            while self.chat_tree.contains_key(&key).unwrap() {
                channel_id = rng.gen_range(1..=u64::MAX);
                key = make_chan_key(guild_id, channel_id);
            }
            channel_id
        };
        let key = make_chan_key(guild_id, channel_id);

        let channel = Channel {
            metadata,
            channel_name,
            is_category,
        };
        let buf = rkyv_ser(&channel);
        cchat_insert!(key / buf);

        // Add from ordering list
        self.update_channel_order_logic(
            guild_id,
            channel_id,
            position.map_or_else(|| Place::between(0, 0), Place::from),
        )?;

        Ok(channel_id)
    }

    /// Calculates all users which can "see" the given user
    pub fn calculate_users_seeing_user(&self, user_id: u64) -> Vec<u64> {
        let prefix = make_guild_list_key_prefix(user_id);
        self.chat_tree
            .scan_prefix(&prefix)
            .map(|res| {
                let (key, _) = res.unwrap();
                let (_, guild_id_raw) = key.split_at(prefix.len());
                let (id_raw, _) = guild_id_raw.split_at(size_of::<u64>());
                // Safety: safe since we split at u64 boundary
                let guild_id = u64::from_be_bytes(unsafe { id_raw.try_into().unwrap_unchecked() });
                self.get_guild_members_logic(guild_id).members
            })
            .flatten()
            .collect()
    }

    /// Adds a guild to a user's guild list
    pub fn add_guild_to_guild_list(&self, user_id: u64, guild_id: u64, homeserver: &str) {
        cchat_insert!(
            [
                make_guild_list_key_prefix(user_id).as_ref(),
                guild_id.to_be_bytes().as_ref(),
                homeserver.as_bytes(),
            ]
            .concat()
                / []
        );
    }

    /// Removes a guild from a user's guild list
    pub fn remove_guild_from_guild_list(&self, user_id: u64, guild_id: u64, homeserver: &str) {
        self.chat_tree
            .remove(&make_guild_list_key(user_id, guild_id, homeserver))
            .unwrap();
    }

    pub fn get_guild_roles_logic(&self, guild_id: u64) -> Vec<RoleWithId> {
        let prefix = make_guild_role_prefix(guild_id);
        self.chat_tree
            .scan_prefix(&prefix)
            .flat_map(|res| {
                let (key, val) = res.unwrap();
                (key.len() == make_guild_role_key(guild_id, 0).len()).then(|| {
                    let role = db::deser_role(val);
                    let role_id = u64::from_be_bytes(unsafe {
                        key.split_at(prefix.len()).1.try_into().unwrap_unchecked()
                    });
                    RoleWithId {
                        role_id,
                        role: Some(role),
                    }
                })
            })
            .collect()
    }

    pub fn get_permissions_logic(
        &self,
        guild_id: u64,
        channel_id: Option<u64>,
        role_id: u64,
    ) -> Vec<(SmolStr, bool)> {
        let get = |prefix: &[u8]| {
            self.chat_tree
                .scan_prefix(prefix)
                .map(|res| {
                    let (key, value) = res.unwrap();
                    let matches_raw = key.split_at(prefix.len()).1;
                    let matches = unsafe { std::str::from_utf8_unchecked(matches_raw) };
                    let ok = value[0] != 0;
                    (matches.into(), ok)
                })
                .collect()
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
    ) -> Result<QueryHasPermissionResponse, ServerError> {
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
            return Err(ServerError::EmptyPermissionQuery);
        }

        Ok(QueryHasPermissionResponse {
            ok: self
                .check_perms(guild_id, channel_id, check_as, &check_for, false)
                .is_ok(),
        })
    }
}
