use std::{convert::TryInto, mem::size_of, ops::Not};

use event::MessageUpdated;
use get_guild_channels_response::Channel;
use harmony_rust_sdk::api::{
    chat::{
        event::{ChannelUpdated, GuildUpdated, LeaveReason},
        *,
    },
    exports::{
        hrpc::{
            encode_protobuf_message, return_print,
            server::{Socket, SocketError, WriteSocket},
            warp::reply::Response,
            Request,
        },
        prost::{bytes::BytesMut, Message},
    },
    harmonytypes::{content, Content, ContentText, Message as HarmonyMessage, Metadata},
    sync::{
        event::{
            Kind as DispatchKind, UserAddedToGuild as SyncUserAddedToGuild,
            UserRemovedFromGuild as SyncUserRemovedFromGuild,
        },
        Event as DispatchEvent,
    },
};
use scherzo_derive::*;
use smol_str::SmolStr;
use tokio::{
    sync::{
        broadcast::{self, Sender as BroadcastSend},
        mpsc::{self, Receiver, UnboundedSender},
    },
    task::JoinHandle,
};
use triomphe::Arc;

use crate::{
    append_list::AppendList,
    db::{self, chat::*, Batch},
    impls::{auth, gen_rand_u64, sync::EventDispatch},
    set_proto_name, ServerError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EventSub {
    Guild(u64),
    Homeserver,
    Actions,
}

#[derive(Clone, Copy)]
struct PermCheck<'a> {
    guild_id: u64,
    channel_id: u64,
    check_for: &'a str,
    must_be_guild_owner: bool,
}

impl<'a> PermCheck<'a> {
    const fn new(
        guild_id: u64,
        channel_id: u64,
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

struct EventContext {
    user_ids: Vec<u64>,
}

impl EventContext {
    const fn new(user_ids: Vec<u64>) -> Self {
        Self { user_ids }
    }

    fn empty() -> Self {
        Self::new(Vec::new())
    }
}

struct EventBroadcast {
    sub: EventSub,
    event: event::Event,
    perm_check: Option<PermCheck<'static>>,
    context: EventContext,
}

pub struct ChatServer {
    valid_sessions: auth::SessionMap,
    chat_tree: ChatTree,
    broadcast_send: BroadcastSend<Arc<EventBroadcast>>,
    dispatch_tx: UnboundedSender<EventDispatch>,
}

impl ChatServer {
    pub fn new(
        chat_tree: ChatTree,
        valid_sessions: auth::SessionMap,
        dispatch_tx: UnboundedSender<EventDispatch>,
    ) -> Self {
        // TODO: is 1000 a fine upper limit? maybe we should make this limit configurable?
        let (tx, _) = broadcast::channel(1000);
        Self {
            valid_sessions,
            chat_tree,
            broadcast_send: tx,
            dispatch_tx,
        }
    }

    fn spawn_event_stream_processor(
        &self,
        user_id: u64,
        mut sub_rx: Receiver<EventSub>,
        mut ws: WriteSocket<Event>,
    ) -> JoinHandle<()> {
        async fn send_event(
            ws: &mut WriteSocket<Event>,
            broadcast: &EventBroadcast,
            user_id: u64,
        ) -> bool {
            ws.send_message(Event {
                event: Some(broadcast.event.clone()),
            })
            .await
            .map_or_else(
                |err| {
                    if !matches!(err, SocketError::ClosedNormally) {
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
            let subs = AppendList::new();

            loop {
                tokio::select! {
                    _ = async {
                        // Push all the subs
                        while let Some(sub) = sub_rx.recv().await {
                            subs.append(sub);
                        }
                    } => { }
                    res = async {
                        // Broadcast all events
                        // TODO: what do we do when RecvError::Lagged?
                        while let Ok(broadcast) = rx.recv().await {
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

                            if subs.iter().any(|val| val.eq(&broadcast.sub)) {
                                if !broadcast.context.user_ids.is_empty() {
                                    if broadcast.context.user_ids.contains(&user_id)
                                        && check_perms()
                                        && send_event(&mut ws, broadcast.as_ref(), user_id).await
                                    {
                                        return true;
                                    }
                                } else if check_perms()
                                    && send_event(&mut ws, broadcast.as_ref(), user_id).await
                                {
                                    return true;
                                }
                            }
                        }

                        false
                    } => {
                        if res {
                            break;
                        }
                    }
                }
            }
        })
    }

    #[inline(always)]
    fn send_event_through_chan(
        &self,
        sub: EventSub,
        event: event::Event,
        perm_check: Option<PermCheck<'static>>,
        context: EventContext,
    ) {
        let broadcast = EventBroadcast {
            sub,
            event,
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
}

#[harmony_rust_sdk::api::exports::hrpc::async_trait]
impl chat_service_server::ChatService for ChatServer {
    type Error = ServerError;

    #[rate(1, 5)]
    async fn create_guild(
        &self,
        request: Request<CreateGuildRequest>,
    ) -> Result<CreateGuildResponse, Self::Error> {
        auth!();

        let (
            CreateGuildRequest {
                metadata,
                guild_name,
                picture_url,
            },
            headers,
            addr,
        ) = request.into_parts();

        let guild_id = {
            let mut guild_id = gen_rand_u64();
            while self
                .chat_tree
                .chat_tree
                .contains_key(&guild_id.to_be_bytes())
                .unwrap()
            {
                guild_id = gen_rand_u64();
            }
            guild_id
        };

        let guild = GetGuildResponse {
            guild_name,
            guild_picture: picture_url,
            guild_owner: user_id,
            metadata,
        };
        let buf = encode_protobuf_message(guild);

        chat_insert!(guild_id.to_be_bytes(), buf);
        chat_insert!(make_member_key(guild_id, user_id), []);

        // Some basic default setup
        let everyone_role_id = self.chat_tree.add_guild_role_logic(
            guild_id,
            Role {
                name: "everyone".to_string(),
                pingable: false,
                ..Default::default()
            },
        )?;
        chat_insert!(
            make_guild_default_role_key(guild_id),
            // [tag:default_role_store]
            everyone_role_id.to_be_bytes(),
        );
        self.chat_tree.add_default_role_to(guild_id, user_id)?;
        let def_perms = PermissionList {
            permissions: ["messages.send", "messages.view"]
                .iter()
                .map(|m| Permission {
                    matches: m.to_string(),
                    mode: permission::Mode::Allow.into(),
                })
                .collect(),
        };
        self.chat_tree
            .set_permissions_logic(guild_id, 0, everyone_role_id, def_perms.clone());
        let channel_id = self
            .create_channel(Request::from_parts((
                CreateChannelRequest {
                    guild_id,
                    channel_name: "general".to_string(),
                    ..Default::default()
                },
                headers,
                addr,
            )))
            .await?
            .channel_id;
        self.chat_tree
            .set_permissions_logic(guild_id, channel_id, everyone_role_id, def_perms);

        match self.chat_tree.local_to_foreign_id(user_id) {
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
                    event::Event::GuildAddedToList(event::GuildAddedToList {
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
    ) -> Result<CreateInviteResponse, Self::Error> {
        auth!();

        let CreateInviteRequest {
            guild_id,
            name,
            possible_uses,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "invites.manage.create", false)?;

        let key = make_invite_key(name.as_str());

        let invite = get_guild_invites_response::Invite {
            possible_uses,
            use_count: 0,
            invite_id: name.clone(),
        };
        let buf = encode_protobuf_message(invite);

        chat_insert!(
            key,
            [guild_id.to_be_bytes().as_ref(), buf.as_ref()].concat(),
        );

        Ok(CreateInviteResponse { name })
    }

    #[rate(5, 5)]
    async fn create_channel(
        &self,
        request: Request<CreateChannelRequest>,
    ) -> Result<CreateChannelResponse, Self::Error> {
        auth!();

        let CreateChannelRequest {
            guild_id,
            channel_name,
            is_category,
            previous_id,
            next_id,
            metadata,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "channels.manage.create", false)?;

        let channel_id = self.chat_tree.create_channel_logic(
            guild_id,
            channel_name.clone(),
            is_category,
            metadata.clone(),
            previous_id,
            next_id,
        )?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::CreatedChannel(event::ChannelCreated {
                guild_id,
                channel_id,
                name: channel_name,
                previous_id,
                next_id,
                is_category,
                metadata,
            }),
            Some(PermCheck::new(guild_id, channel_id, "messages.view", false)),
            EventContext::empty(),
        );

        Ok(CreateChannelResponse { channel_id })
    }

    #[rate(5, 5)]
    async fn create_emote_pack(
        &self,
        request: Request<CreateEmotePackRequest>,
    ) -> Result<CreateEmotePackResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    #[rate(15, 5)]
    async fn get_guild_list(
        &self,
        request: Request<GetGuildListRequest>,
    ) -> Result<GetGuildListResponse, Self::Error> {
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

                get_guild_list_response::GuildListEntry {
                    guild_id,
                    host: host.to_string(),
                }
            })
            .collect();

        Ok(GetGuildListResponse { guilds })
    }

    #[rate(15, 5)]
    async fn get_guild(
        &self,
        request: Request<GetGuildRequest>,
    ) -> Result<GetGuildResponse, Self::Error> {
        auth!();

        let GetGuildRequest { guild_id } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        self.chat_tree.get_guild_logic(guild_id)
    }

    #[rate(15, 5)]
    async fn get_guild_invites(
        &self,
        request: Request<GetGuildInvitesRequest>,
    ) -> Result<GetGuildInvitesResponse, Self::Error> {
        auth!();

        let GetGuildInvitesRequest { guild_id } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "invites.view", false)?;

        Ok(self.chat_tree.get_guild_invites_logic(guild_id))
    }

    #[rate(15, 5)]
    async fn get_guild_members(
        &self,
        request: Request<GetGuildMembersRequest>,
    ) -> Result<GetGuildMembersResponse, Self::Error> {
        auth!();

        let GetGuildMembersRequest { guild_id } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        Ok(self.chat_tree.get_guild_members_logic(guild_id))
    }

    #[rate(15, 5)]
    async fn get_guild_channels(
        &self,
        request: Request<GetGuildChannelsRequest>,
    ) -> Result<GetGuildChannelsResponse, Self::Error> {
        auth!();

        let GetGuildChannelsRequest { guild_id } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        Ok(self.chat_tree.get_guild_channels_logic(guild_id, user_id))
    }

    #[rate(10, 5)]
    async fn get_channel_messages(
        &self,
        request: Request<GetChannelMessagesRequest>,
    ) -> Result<GetChannelMessagesResponse, Self::Error> {
        auth!();

        let GetChannelMessagesRequest {
            guild_id,
            channel_id,
            before_message,
        } = request.into_parts().0;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree
            .check_perms(guild_id, channel_id, user_id, "messages.view", false)?;

        Ok(self
            .chat_tree
            .get_channel_messages_logic(guild_id, channel_id, before_message))
    }

    #[rate(10, 5)]
    async fn get_message(
        &self,
        request: Request<GetMessageRequest>,
    ) -> Result<GetMessageResponse, Self::Error> {
        auth!();

        let request = request.into_parts().0;

        let GetMessageRequest {
            guild_id,
            channel_id,
            message_id,
        } = request;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree
            .check_perms(guild_id, channel_id, user_id, "messages.view", false)?;

        let message = Some(
            self.chat_tree
                .get_message_logic(guild_id, channel_id, message_id)?
                .0,
        );

        Ok(GetMessageResponse { message })
    }

    #[rate(10, 5)]
    async fn get_emote_packs(
        &self,
        request: Request<GetEmotePacksRequest>,
    ) -> Result<GetEmotePacksResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    #[rate(20, 5)]
    async fn get_emote_pack_emotes(
        &self,
        request: Request<GetEmotePackEmotesRequest>,
    ) -> Result<GetEmotePackEmotesResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    #[rate(2, 5)]
    async fn update_guild_information(
        &self,
        request: Request<UpdateGuildInformationRequest>,
    ) -> Result<(), Self::Error> {
        auth!();

        let UpdateGuildInformationRequest {
            guild_id,
            new_guild_name,
            update_guild_name,
            new_guild_picture,
            update_guild_picture,
            metadata,
            update_metadata,
        } = request.into_parts().0;

        let key = guild_id.to_be_bytes();
        let mut guild_info = chat_get!(key)
            .map(db::deser_guild)
            .ok_or(ServerError::NoSuchGuild(guild_id))?;

        if !self.chat_tree.is_user_in_guild(guild_id, user_id) {
            return Err(ServerError::UserNotInGuild { guild_id, user_id });
        }

        self.chat_tree.check_perms(
            guild_id,
            0,
            user_id,
            "guild.manage.change-information",
            false,
        )?;

        if update_guild_name {
            guild_info.guild_name = new_guild_name.clone();
        }
        if update_guild_picture {
            guild_info.guild_picture = new_guild_picture.clone();
        }
        if update_metadata {
            guild_info.metadata = metadata.clone();
        }

        let buf = encode_protobuf_message(guild_info);
        chat_insert!(key, buf);

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::EditedGuild(GuildUpdated {
                guild_id,
                update_picture: update_guild_picture,
                picture: new_guild_picture,
                update_name: update_guild_name,
                name: new_guild_name,
                update_metadata,
                metadata,
            }),
            None,
            EventContext::empty(),
        );

        Ok(())
    }

    #[rate(2, 5)]
    async fn update_channel_information(
        &self,
        request: Request<UpdateChannelInformationRequest>,
    ) -> Result<(), Self::Error> {
        auth!();

        let UpdateChannelInformationRequest {
            guild_id,
            channel_id,
            name,
            update_name,
            metadata,
            update_metadata,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.check_perms(
            guild_id,
            channel_id,
            user_id,
            "channels.manage.change-information",
            false,
        )?;

        let key = make_chan_key(guild_id, channel_id);
        let mut chan_info =
            chat_get!(key)
                .map(db::deser_chan)
                .ok_or(ServerError::NoSuchChannel {
                    guild_id,
                    channel_id,
                })?;

        if update_name {
            chan_info.channel_name = name.clone();
        }
        if update_metadata {
            chan_info.metadata = metadata.clone();
        }

        let buf = encode_protobuf_message(chan_info);
        chat_insert!(key, buf);

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::EditedChannel(ChannelUpdated {
                guild_id,
                channel_id,
                name,
                update_name,
                metadata,
                update_metadata,
                ..Default::default()
            }),
            Some(PermCheck::new(guild_id, channel_id, "messages.view", false)),
            EventContext::empty(),
        );

        Ok(())
    }

    #[rate(2, 5)]
    async fn update_channel_order(
        &self,
        request: Request<UpdateChannelOrderRequest>,
    ) -> Result<(), Self::Error> {
        auth!();

        let UpdateChannelOrderRequest {
            guild_id,
            channel_id,
            previous_id,
            next_id,
        } = request.into_parts().0;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree
            .check_perms(guild_id, channel_id, user_id, "channels.manage.move", false)?;

        self.chat_tree
            .update_channel_order_logic(guild_id, channel_id, previous_id, next_id)?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::EditedChannel(ChannelUpdated {
                guild_id,
                channel_id,
                previous_id,
                next_id,
                update_order: true,
                ..Default::default()
            }),
            Some(PermCheck::new(guild_id, channel_id, "messages.view", false)),
            EventContext::empty(),
        );

        Ok(())
    }

    #[rate(2, 5)]
    async fn update_message_text(
        &self,
        request: Request<UpdateMessageTextRequest>,
    ) -> Result<(), Self::Error> {
        auth!();

        let request = request.into_parts().0;

        let UpdateMessageTextRequest {
            guild_id,
            channel_id,
            message_id,
            new_content,
        } = request;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree
            .check_perms(guild_id, channel_id, user_id, "messages.send", false)?;

        let (mut message, key) = self
            .chat_tree
            .get_message_logic(guild_id, channel_id, message_id)?;

        let msg_content = if let Some(content) = &mut message.content {
            content
        } else {
            message.content = Some(Content::default());
            message.content.as_mut().unwrap()
        };
        msg_content.content = Some(content::Content::TextMessage(ContentText {
            content: new_content.clone(),
        }));

        let edited_at = Some(std::time::SystemTime::now().into());
        message.edited_at = edited_at.clone();

        let buf = encode_protobuf_message(message);
        chat_insert!(key, buf);

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::EditedMessage(Box::new(MessageUpdated {
                guild_id,
                channel_id,
                message_id,
                edited_at,
                content: new_content,
            })),
            Some(PermCheck::new(guild_id, channel_id, "messages.view", false)),
            EventContext::empty(),
        );

        Ok(())
    }

    #[rate(20, 5)]
    async fn add_emote_to_pack(
        &self,
        request: Request<AddEmoteToPackRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    #[rate(1, 15)]
    async fn delete_guild(&self, request: Request<DeleteGuildRequest>) -> Result<(), Self::Error> {
        auth!();

        let DeleteGuildRequest { guild_id } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "guild.manage.delete", false)?;

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
            event::Event::DeletedGuild(event::GuildDeleted { guild_id }),
            None,
            EventContext::empty(),
        );

        let mut local_ids = Vec::new();
        for member_id in guild_members {
            match self.chat_tree.local_to_foreign_id(member_id) {
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
            event::Event::GuildRemovedFromList(event::GuildRemovedFromList {
                guild_id,
                homeserver: String::new(),
            }),
            None,
            EventContext::new(local_ids),
        );

        Ok(())
    }

    #[rate(5, 5)]
    async fn delete_invite(
        &self,
        request: Request<DeleteInviteRequest>,
    ) -> Result<(), Self::Error> {
        auth!();

        let DeleteInviteRequest {
            guild_id,
            invite_id,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "invites.manage.delete", false)?;

        chat_remove!(make_invite_key(invite_id.as_str()));

        Ok(())
    }

    #[rate(5, 5)]
    async fn delete_channel(
        &self,
        request: Request<DeleteChannelRequest>,
    ) -> Result<(), Self::Error> {
        auth!();

        let DeleteChannelRequest {
            guild_id,
            channel_id,
        } = request.into_parts().0;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree.check_perms(
            guild_id,
            channel_id,
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
        batch.insert(&key, serialized_ordering);
        self.chat_tree.chat_tree.apply_batch(batch).unwrap();

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::DeletedChannel(event::ChannelDeleted {
                guild_id,
                channel_id,
            }),
            None,
            EventContext::empty(),
        );

        Ok(())
    }

    #[rate(5, 5)]
    async fn delete_message(
        &self,
        request: Request<DeleteMessageRequest>,
    ) -> Result<(), Self::Error> {
        auth!();

        let DeleteMessageRequest {
            guild_id,
            channel_id,
            message_id,
        } = request.into_parts().0;

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
                channel_id,
                user_id,
                "messages.manage.delete",
                false,
            )?;
        }

        chat_remove!(make_msg_key(guild_id, channel_id, message_id));

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::DeletedMessage(event::MessageDeleted {
                guild_id,
                channel_id,
                message_id,
            }),
            Some(PermCheck::new(guild_id, channel_id, "messages.view", false)),
            EventContext::empty(),
        );

        Ok(())
    }

    #[rate(20, 5)]
    async fn delete_emote_from_pack(
        &self,
        request: Request<DeleteEmoteFromPackRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    #[rate(5, 5)]
    async fn delete_emote_pack(
        &self,
        request: Request<DeleteEmotePackRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    #[rate(10, 5)]
    async fn dequip_emote_pack(
        &self,
        request: Request<DequipEmotePackRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    #[rate(5, 5)]
    async fn join_guild(
        &self,
        request: Request<JoinGuildRequest>,
    ) -> Result<JoinGuildResponse, Self::Error> {
        auth!();

        let JoinGuildRequest { invite_id } = request.into_parts().0;
        let key = make_invite_key(invite_id.as_str());

        let (guild_id, mut invite) = chat_get!(key)
            .map(db::deser_invite_entry)
            .ok_or_else(|| ServerError::NoSuchInvite(invite_id.into()))?;

        if self.chat_tree.is_user_banned_in_guild(guild_id, user_id) {
            return Err(ServerError::UserBanned);
        }

        if self.chat_tree.is_user_in_guild(guild_id, user_id) {
            return Err(ServerError::UserAlreadyInGuild);
        }

        let is_infinite = invite.possible_uses == -1;

        if is_infinite.not() && invite.use_count >= invite.possible_uses {
            return Err(ServerError::InviteExpired);
        }

        chat_insert!(make_member_key(guild_id, user_id), []);
        self.chat_tree.add_default_role_to(guild_id, user_id)?;
        invite.use_count += 1;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::JoinedMember(event::MemberJoined {
                guild_id,
                member_id: user_id,
            }),
            None,
            EventContext::empty(),
        );

        match self.chat_tree.local_to_foreign_id(user_id) {
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
                    event::Event::GuildAddedToList(event::GuildAddedToList {
                        guild_id,
                        homeserver: String::new(),
                    }),
                    None,
                    EventContext::new(vec![user_id]),
                );
            }
        }

        let buf = encode_protobuf_message(invite);
        chat_insert!(
            key,
            [guild_id.to_be_bytes().as_ref(), buf.as_ref()].concat(),
        );

        Ok(JoinGuildResponse { guild_id })
    }

    #[rate(5, 5)]
    async fn leave_guild(&self, request: Request<LeaveGuildRequest>) -> Result<(), Self::Error> {
        auth!();

        let LeaveGuildRequest { guild_id } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        chat_remove!(make_member_key(guild_id, user_id));

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::LeftMember(event::MemberLeft {
                guild_id,
                member_id: user_id,
                leave_reason: LeaveReason::Willingly.into(),
            }),
            None,
            EventContext::empty(),
        );

        match self.chat_tree.local_to_foreign_id(user_id) {
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
                    event::Event::GuildRemovedFromList(event::GuildRemovedFromList {
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

    #[rate(20, 5)]
    async fn trigger_action(
        &self,
        request: Request<TriggerActionRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    #[rate(50, 10)]
    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<SendMessageResponse, Self::Error> {
        auth!();

        let SendMessageRequest {
            guild_id,
            channel_id,
            content,
            in_reply_to,
            overrides,
            echo_id,
            metadata,
        } = request.into_parts().0;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree
            .check_perms(guild_id, channel_id, user_id, "messages.send", false)?;

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

        let created_at = Some(std::time::SystemTime::now().into());
        let edited_at = None;

        let message = HarmonyMessage {
            metadata,
            guild_id,
            channel_id,
            message_id,
            author_id: user_id,
            created_at,
            edited_at,
            content,
            in_reply_to,
            overrides,
        };

        let mut buf = BytesMut::with_capacity(message.encoded_len());
        // This can never fail, so we ignore the result
        let _ = message.encode(&mut buf);
        chat_insert!(key, buf);

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::SentMessage(Box::new(event::MessageSent {
                echo_id,
                message: Some(message),
            })),
            Some(PermCheck::new(guild_id, channel_id, "messages.view", false)),
            EventContext::empty(),
        );

        Ok(SendMessageResponse { message_id })
    }

    #[rate(30, 5)]
    async fn query_has_permission(
        &self,
        request: Request<QueryPermissionsRequest>,
    ) -> Result<QueryPermissionsResponse, Self::Error> {
        auth!();

        let QueryPermissionsRequest {
            guild_id,
            channel_id,
            check_for,
            r#as,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;

        let as_other = r#as != 0;
        let check_as = if as_other { r#as } else { user_id };

        if as_other {
            self.chat_tree.check_perms(
                guild_id,
                channel_id,
                user_id,
                "permissions.query",
                false,
            )?;
        }

        if check_for.is_empty() {
            return Err(ServerError::EmptyPermissionQuery);
        }

        Ok(QueryPermissionsResponse {
            ok: self
                .chat_tree
                .check_perms(guild_id, channel_id, check_as, &check_for, false)
                .is_ok(),
        })
    }

    #[rate(10, 5)]
    async fn set_permissions(
        &self,
        request: Request<SetPermissionsRequest>,
    ) -> Result<(), Self::Error> {
        auth!();

        let SetPermissionsRequest {
            guild_id,
            channel_id,
            role_id,
            perms,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.check_perms(
            guild_id,
            channel_id,
            user_id,
            "permissions.manage.set",
            false,
        )?;

        if let Some(perms) = perms {
            self.chat_tree
                .set_permissions_logic(guild_id, channel_id, role_id, perms);
            Ok(())
        } else {
            Err(ServerError::NoPermissionsSpecified)
        }
    }

    #[rate(10, 5)]
    async fn get_permissions(
        &self,
        request: Request<GetPermissionsRequest>,
    ) -> Result<GetPermissionsResponse, Self::Error> {
        auth!();

        let GetPermissionsRequest {
            guild_id,
            channel_id,
            role_id,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree.check_perms(
            guild_id,
            channel_id,
            user_id,
            "permissions.manage.get",
            false,
        )?;

        let perms =
            chat_get!(make_guild_role_perms_key(guild_id, role_id)).map(db::deser_perm_list);

        Ok(GetPermissionsResponse { perms })
    }

    #[rate(10, 5)]
    async fn move_role(
        &self,
        request: Request<MoveRoleRequest>,
    ) -> Result<MoveRoleResponse, Self::Error> {
        auth!();

        let MoveRoleRequest {
            guild_id,
            role_id,
            before_id,
            after_id,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "roles.manage", false)?;

        if !self.chat_tree.does_role_exist(guild_id, role_id) {
            return Err(ServerError::NoSuchRole { guild_id, role_id });
        }

        self.chat_tree
            .move_role_logic(guild_id, role_id, before_id, after_id)?;

        Ok(MoveRoleResponse {})
    }

    #[rate(10, 5)]
    async fn get_guild_roles(
        &self,
        request: Request<GetGuildRolesRequest>,
    ) -> Result<GetGuildRolesResponse, Self::Error> {
        auth!();

        let GetGuildRolesRequest { guild_id } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "roles.get", false)?;

        let prefix = make_guild_role_prefix(guild_id);
        let roles = self
            .chat_tree
            .chat_tree
            .scan_prefix(&prefix)
            .flat_map(|res| {
                let (key, val) = res.unwrap();
                (key.len() == prefix.len() + size_of::<u64>()).then(|| db::deser_role(val))
            })
            .collect();

        Ok(GetGuildRolesResponse { roles })
    }

    #[rate(10, 5)]
    async fn add_guild_role(
        &self,
        request: Request<AddGuildRoleRequest>,
    ) -> Result<AddGuildRoleResponse, Self::Error> {
        auth!();

        let AddGuildRoleRequest { guild_id, role } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "roles.manage", false)?;

        if let Some(role) = role {
            let role_id = self.chat_tree.add_guild_role_logic(guild_id, role)?;
            Ok(AddGuildRoleResponse { role_id })
        } else {
            Err(ServerError::NoRoleSpecified)
        }
    }

    #[rate(10, 5)]
    async fn modify_guild_role(
        &self,
        request: Request<ModifyGuildRoleRequest>,
    ) -> Result<(), Self::Error> {
        auth!();

        let ModifyGuildRoleRequest {
            guild_id,
            role: maybe_role,
            modify_name,
            modify_color,
            modify_hoist,
            modify_pingable,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "roles.manage", false)?;

        if let Some(new_role) = maybe_role {
            let role_id = new_role.role_id;
            let key = make_guild_role_key(guild_id, role_id);
            let mut role = chat_get!(key)
                .map(db::deser_role)
                .ok_or(ServerError::NoSuchRole { guild_id, role_id })?;

            if modify_name {
                role.name = new_role.name;
            }
            if modify_color {
                role.color = new_role.color;
            }
            if modify_hoist {
                role.hoist = new_role.hoist;
            }
            if modify_pingable {
                role.pingable = new_role.pingable;
            }

            let ser_role = encode_protobuf_message(role);
            chat_insert!(key, ser_role);
            Ok(())
        } else {
            Err(ServerError::NoRoleSpecified)
        }
    }

    #[rate(10, 5)]
    async fn delete_guild_role(
        &self,
        request: Request<DeleteGuildRoleRequest>,
    ) -> Result<(), Self::Error> {
        auth!();

        let DeleteGuildRoleRequest { guild_id, role_id } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "roles.manage", false)?;

        chat_remove!(make_guild_role_key(guild_id, role_id))
            .ok_or(ServerError::NoSuchRole { guild_id, role_id })
            .map(|_| ())
    }

    #[rate(10, 5)]
    async fn manage_user_roles(
        &self,
        request: Request<ManageUserRolesRequest>,
    ) -> Result<(), Self::Error> {
        auth!();

        let ManageUserRolesRequest {
            guild_id,
            user_id: user_to_manage,
            give_role_ids,
            take_role_ids,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        if !self.chat_tree.is_user_in_guild(guild_id, user_to_manage) {
            return Err(ServerError::UserNotInGuild {
                guild_id,
                user_id: user_to_manage,
            });
        }
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "roles.user.manage", false)?;
        let user_to_manage = (user_to_manage != 0)
            .then(|| user_to_manage)
            .unwrap_or(user_id);

        self.chat_tree.manage_user_roles_logic(
            guild_id,
            user_to_manage,
            give_role_ids,
            take_role_ids,
        )
    }

    #[rate(10, 5)]
    async fn get_user_roles(
        &self,
        request: Request<GetUserRolesRequest>,
    ) -> Result<GetUserRolesResponse, Self::Error> {
        auth!();

        let GetUserRolesRequest {
            guild_id,
            user_id: user_to_fetch,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        if !self.chat_tree.is_user_in_guild(guild_id, user_to_fetch) {
            return Err(ServerError::UserNotInGuild {
                guild_id,
                user_id: user_to_fetch,
            });
        }
        let fetch_user = (user_to_fetch == 0)
            .then(|| user_id)
            .unwrap_or(user_to_fetch);
        if fetch_user != user_id {
            self.chat_tree
                .check_perms(guild_id, 0, user_id, "roles.user.get", false)?;
        }

        let roles = self.chat_tree.get_user_roles_logic(guild_id, fetch_user);

        Ok(GetUserRolesResponse { roles })
    }

    fn stream_events_on_upgrade(&self, response: Response) -> Response {
        set_proto_name(response)
    }

    type StreamEventsValidationType = u64;

    async fn stream_events_validation(
        &self,
        request: Request<Option<StreamEventsRequest>>,
    ) -> Result<u64, Self::Error> {
        auth!();
        Ok(user_id)
    }

    #[rate(1, 5)]
    async fn stream_events(&self, user_id: u64, socket: Socket<StreamEventsRequest, Event>) {
        let (mut rs, ws) = socket.split();
        let (sub_tx, sub_rx) = mpsc::channel(64);
        let chat_tree = self.chat_tree.clone();

        let send_loop = self.spawn_event_stream_processor(user_id, sub_rx, ws);
        let recv_loop = async move {
            loop {
                return_print!(rs.receive_message().await, |maybe_req| {
                    if let Some(req) = maybe_req.and_then(|r| r.request) {
                        use stream_events_request::*;

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
    }

    #[rate(32, 10)]
    async fn get_user(
        &self,
        request: Request<GetUserRequest>,
    ) -> Result<GetUserResponse, Self::Error> {
        auth!();

        let GetUserRequest { user_id } = request.into_parts().0;

        self.chat_tree.get_user_logic(user_id)
    }

    #[rate(5, 5)]
    async fn get_user_bulk(
        &self,
        request: Request<GetUserBulkRequest>,
    ) -> Result<GetUserBulkResponse, Self::Error> {
        auth!();

        let GetUserBulkRequest { user_ids } = request.into_parts().0;

        let mut profiles = Vec::with_capacity(user_ids.len());

        for (id, key) in user_ids
            .into_iter()
            .map(|id| (id, make_user_profile_key(id)))
        {
            if let Some(raw) = chat_get!(key) {
                profiles.push(db::deser_profile(raw));
            } else {
                return Err(ServerError::NoSuchUser(id));
            }
        }

        Ok(GetUserBulkResponse { users: profiles })
    }

    #[rate(4, 1)]
    async fn get_user_metadata(
        &self,
        request: Request<GetUserMetadataRequest>,
    ) -> Result<GetUserMetadataResponse, Self::Error> {
        auth!();

        let GetUserMetadataRequest { app_id } = request.into_parts().0;
        let metadata = chat_get!(make_user_metadata_key(user_id, &app_id)).map_or_else(
            String::default,
            |raw| unsafe {
                // Safety: this can never cause UB since we don't store non UTF-8 user metadata
                String::from_utf8_unchecked(raw.to_vec())
            },
        );

        Ok(GetUserMetadataResponse { metadata })
    }

    #[rate(4, 5)]
    async fn profile_update(
        &self,
        request: Request<ProfileUpdateRequest>,
    ) -> Result<(), Self::Error> {
        auth!();

        let ProfileUpdateRequest {
            new_username,
            update_username,
            new_avatar,
            update_avatar,
            new_status,
            update_status,
            is_bot,
            update_is_bot,
        } = request.into_parts().0;

        let key = make_user_profile_key(user_id);

        let mut profile = chat_get!(key).map_or_else(GetUserResponse::default, db::deser_profile);

        if update_username {
            profile.user_name = new_username.clone();
        }
        if update_avatar {
            profile.user_avatar = new_avatar.clone();
        }
        if update_status {
            profile.user_status = new_status;
        }
        if update_is_bot {
            profile.is_bot = is_bot;
        }

        let buf = encode_protobuf_message(profile);
        chat_insert!(key, buf);

        self.send_event_through_chan(
            EventSub::Homeserver,
            event::Event::ProfileUpdated(event::ProfileUpdated {
                user_id,
                new_username,
                update_username,
                new_avatar,
                update_avatar,
                new_status,
                update_status,
                is_bot,
                update_is_bot,
            }),
            None,
            EventContext::new(self.chat_tree.calculate_users_seeing_user(user_id)),
        );

        Ok(())
    }

    #[rate(4, 5)]
    async fn typing(&self, request: Request<TypingRequest>) -> Result<(), Self::Error> {
        auth!();

        let TypingRequest {
            guild_id,
            channel_id,
        } = request.into_parts().0;

        self.chat_tree
            .check_guild_user_channel(guild_id, user_id, channel_id)?;
        self.chat_tree
            .check_perms(guild_id, channel_id, user_id, "messages.send", false)?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::Typing(event::Typing {
                user_id,
                guild_id,
                channel_id,
            }),
            Some(PermCheck::new(guild_id, channel_id, "messages.view", false)),
            EventContext::empty(),
        );

        Ok(())
    }

    #[rate(2, 5)]
    async fn preview_guild(
        &self,
        request: Request<PreviewGuildRequest>,
    ) -> Result<PreviewGuildResponse, Self::Error> {
        auth!();

        let PreviewGuildRequest { invite_id } = request.into_parts().0;

        let key = make_invite_key(&invite_id);
        let guild_id = chat_get!(key)
            .ok_or_else(|| ServerError::NoSuchInvite(invite_id.into()))
            .map(|raw| db::deser_invite_entry_guild_id(&raw))?;
        let guild = self.chat_tree.get_guild_logic(guild_id)?;
        let member_count = self
            .chat_tree
            .chat_tree
            .scan_prefix(&make_guild_mem_prefix(guild_id))
            .count();

        Ok(PreviewGuildResponse {
            name: guild.guild_name,
            avatar: guild.guild_picture,
            member_count: member_count as u64,
        })
    }

    #[rate(4, 5)]
    async fn ban_user(&self, request: Request<BanUserRequest>) -> Result<(), Self::Error> {
        auth!();

        let BanUserRequest {
            guild_id,
            user_id: user_to_ban,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        if !self.chat_tree.is_user_in_guild(guild_id, user_to_ban) {
            return Err(ServerError::UserNotInGuild {
                guild_id,
                user_id: user_to_ban,
            });
        }
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "user.manage.ban", false)?;

        self.chat_tree.kick_user_logic(guild_id, user_to_ban);

        chat_insert!(
            make_banned_member_key(guild_id, user_to_ban),
            get_time_secs().to_be_bytes(),
        );

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::LeftMember(event::MemberLeft {
                guild_id,
                member_id: user_to_ban,
                leave_reason: LeaveReason::Banned.into(),
            }),
            None,
            EventContext::empty(),
        );

        Ok(())
    }

    #[rate(4, 5)]
    async fn kick_user(&self, request: Request<KickUserRequest>) -> Result<(), Self::Error> {
        auth!();

        let KickUserRequest {
            guild_id,
            user_id: user_to_kick,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        if !self.chat_tree.is_user_in_guild(guild_id, user_to_kick) {
            return Err(ServerError::UserNotInGuild {
                guild_id,
                user_id: user_to_kick,
            });
        }
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "user.manage.kick", false)?;

        self.chat_tree.kick_user_logic(guild_id, user_to_kick);

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::LeftMember(event::MemberLeft {
                guild_id,
                member_id: user_to_kick,
                leave_reason: LeaveReason::Kicked.into(),
            }),
            None,
            EventContext::empty(),
        );

        Ok(())
    }

    #[rate(4, 5)]
    async fn unban_user(&self, request: Request<UnbanUserRequest>) -> Result<(), Self::Error> {
        auth!();

        let UnbanUserRequest {
            guild_id,
            user_id: user_to_unban,
        } = request.into_parts().0;

        self.chat_tree.check_guild_user(guild_id, user_id)?;
        self.chat_tree
            .check_perms(guild_id, 0, user_id, "user.manage.unban", false)?;

        chat_remove!(make_banned_member_key(guild_id, user_to_unban));

        Ok(())
    }
}

#[derive(Clone)]
pub struct ChatTree {
    pub chat_tree: db::ArcTree,
}

impl ChatTree {
    pub fn is_user_in_guild(&self, guild_id: u64, user_id: u64) -> bool {
        self.chat_tree
            .contains_key(&make_member_key(guild_id, user_id))
            .unwrap()
    }

    pub fn does_user_exist(&self, user_id: u64) -> bool {
        self.chat_tree
            .contains_key(&make_user_profile_key(user_id))
            .unwrap()
    }

    pub fn does_guild_exist(&self, guild_id: u64) -> bool {
        self.chat_tree
            .contains_key(&guild_id.to_be_bytes())
            .unwrap()
    }

    pub fn does_channel_exist(&self, guild_id: u64, channel_id: u64) -> bool {
        self.chat_tree
            .contains_key(&make_chan_key(guild_id, channel_id))
            .unwrap()
    }

    pub fn does_role_exist(&self, guild_id: u64, role_id: u64) -> bool {
        self.chat_tree
            .contains_key(&make_guild_role_key(guild_id, role_id))
            .unwrap()
    }

    pub fn is_user_banned_in_guild(&self, guild_id: u64, user_id: u64) -> bool {
        self.chat_tree
            .contains_key(&make_banned_member_key(guild_id, user_id))
            .unwrap()
    }

    pub fn is_user_guild_owner(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<bool, <ChatServer as chat_service_server::ChatService>::Error> {
        self.get_guild_logic(guild_id)
            .map(|info| info.guild_owner == user_id)
    }

    pub fn check_guild_user_channel(
        &self,
        guild_id: u64,
        user_id: u64,
        channel_id: u64,
    ) -> Result<(), <ChatServer as chat_service_server::ChatService>::Error> {
        self.check_guild_user(guild_id, user_id)?;
        (channel_id == 0 || !self.does_channel_exist(guild_id, channel_id))
            .then(|| ())
            .ok_or(ServerError::NoSuchChannel {
                guild_id,
                channel_id,
            })
    }

    pub fn check_guild_user(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<(), <ChatServer as chat_service_server::ChatService>::Error> {
        self.check_guild(guild_id)?;
        (user_id == 0 || !self.is_user_in_guild(guild_id, user_id))
            .then(|| ())
            .ok_or(ServerError::UserNotInGuild { guild_id, user_id })
    }

    pub fn check_guild(
        &self,
        guild_id: u64,
    ) -> Result<(), <ChatServer as chat_service_server::ChatService>::Error> {
        (guild_id == 0 || !self.does_guild_exist(guild_id))
            .then(|| ())
            .ok_or(ServerError::NoSuchGuild(guild_id))
    }

    pub fn check_user(
        &self,
        user_id: u64,
    ) -> Result<(), <ChatServer as chat_service_server::ChatService>::Error> {
        (user_id == 0 || !self.does_user_exist(user_id))
            .then(|| ())
            .ok_or(ServerError::NoSuchUser(user_id))
    }

    pub fn get_message_logic(
        &self,
        guild_id: u64,
        channel_id: u64,
        message_id: u64,
    ) -> Result<(HarmonyMessage, [u8; 26]), <ChatServer as chat_service_server::ChatService>::Error>
    {
        let key = make_msg_key(guild_id, channel_id, message_id);
        cchat_get!(key)
            .map(|msg| (db::deser_message(msg), key))
            .ok_or(ServerError::NoSuchMessage {
                guild_id,
                channel_id,
                message_id,
            })
    }

    pub fn get_user_logic(
        &self,
        user_id: u64,
    ) -> Result<GetUserResponse, <ChatServer as chat_service_server::ChatService>::Error> {
        cchat_get!(make_user_profile_key(user_id))
            .map(db::deser_profile)
            .ok_or(ServerError::NoSuchUser(user_id))
    }

    pub fn get_guild_logic(
        &self,
        guild_id: u64,
    ) -> Result<GetGuildResponse, <ChatServer as chat_service_server::ChatService>::Error> {
        cchat_get!(guild_id.to_be_bytes())
            .map(db::deser_guild)
            .ok_or(ServerError::NoSuchGuild(guild_id))
    }

    pub fn get_guild_invites_logic(&self, guild_id: u64) -> GetGuildInvitesResponse {
        let invites = self
            .chat_tree
            .scan_prefix(b"invite_")
            .map(|res| {
                let (_, value) = res.unwrap();
                let (id_raw, invite_raw) = value.split_at(size_of::<u64>());
                // Safety: this unwrap cannot fail since we split at u64 boundary
                let id = u64::from_be_bytes(unsafe { id_raw.try_into().unwrap_unchecked() });
                let invite = db::deser_invite(invite_raw.into());
                (id, invite)
            })
            .filter_map(|(id, invite)| if guild_id == id { Some(invite) } else { None })
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
                        (user_id == 0
                            || self
                                .check_perms(
                                    guild_id,
                                    u64::from_be_bytes(
                                        // Safety: this unwrap is safe since we check if it's a valid u64 beforehand
                                        unsafe {
                                            key.split_at(prefix.len())
                                                .1
                                                .try_into()
                                                .unwrap_unchecked()
                                        },
                                    ),
                                    user_id,
                                    "messages.view",
                                    false,
                                )
                                .is_ok())
                        // Safety: this unwrap is safe since we only store valid Channel message
                        .then(|| unsafe { Channel::decode(value.as_ref()).unwrap_unchecked() })
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();

        if channels.is_empty() {
            return GetGuildChannelsResponse {
                channels: Vec::new(),
            };
        }

        let ordering_raw = cchat_get!(make_guild_chan_ordering_key(guild_id)).unwrap_or_default();
        for (order_index, order_id) in db::make_u64_iter_logic(ordering_raw.as_ref()).enumerate() {
            if let Some(index) = channels.iter().position(|chan| chan.channel_id == order_id) {
                channels.swap(order_index, index);
            }
        }

        GetGuildChannelsResponse { channels }
    }

    #[inline(always)]
    pub fn get_list_u64_logic(&self, key: &[u8]) -> Vec<u64> {
        db::make_u64_iter_logic(cchat_get!(key).unwrap_or_default().as_ref()).collect()
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
        previous_id: u64,
        next_id: u64,
    ) -> Result<(), <ChatServer as chat_service_server::ChatService>::Error> {
        self.update_order_logic(
            role_id,
            previous_id,
            next_id,
            |role_id| {
                self.does_role_exist(guild_id, role_id)
                    .then(|| ())
                    .ok_or(ServerError::NoSuchRole { guild_id, role_id })
            },
            &make_guild_role_ordering_key(guild_id),
        )
    }

    pub fn update_channel_order_logic(
        &self,
        guild_id: u64,
        channel_id: u64,
        previous_id: u64,
        next_id: u64,
    ) -> Result<(), <ChatServer as chat_service_server::ChatService>::Error> {
        self.update_order_logic(
            channel_id,
            previous_id,
            next_id,
            |channel_id| {
                self.does_channel_exist(guild_id, channel_id)
                    .then(|| ())
                    .ok_or(ServerError::NoSuchChannel {
                        guild_id,
                        channel_id,
                    })
            },
            &make_guild_chan_ordering_key(guild_id),
        )
    }

    #[inline(always)]
    pub fn update_order_logic(
        &self,
        id: u64,
        previous_id: u64,
        next_id: u64,
        check_exists: impl Fn(u64) -> Result<(), ServerError>,
        key: &[u8],
    ) -> Result<(), <ChatServer as chat_service_server::ChatService>::Error> {
        let ser_ord_put = |ordering| {
            let serialized_ordering = self.serialize_list_u64_logic(ordering);
            cchat_insert!(key, serialized_ordering);
        };

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
            if let Some(channel_index) = ordering.iter().position(|oid| id.eq(oid)) {
                ordering.swap(channel_index, index);
            } else {
                ordering.insert(index, id);
            }
        };

        if let Some(index) = maybe_ord_index(previous_id) {
            maybe_replace_with(&mut ordering, index + 1);
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
        before_message: u64,
    ) -> GetChannelMessagesResponse {
        let get_messages = |to: u64| {
            let from = to.saturating_sub(25);

            let from_key = make_msg_key(guild_id, channel_id, from);
            let to_key = make_msg_key(guild_id, channel_id, to);

            let messages = self
                .chat_tree
                .range((&from_key)..=(&to_key))
                .rev()
                .map(|res| {
                    let (_, value) = res.unwrap();
                    db::deser_message(value)
                })
                .collect();

            GetChannelMessagesResponse {
                reached_top: from == 0,
                messages,
            }
        };

        let prefix = make_msg_prefix(guild_id, channel_id);
        (before_message == 0)
            .then(|| {
                self.chat_tree.scan_prefix(&prefix).last().map_or_else(
                    || GetChannelMessagesResponse {
                        reached_top: true,
                        messages: Vec::new(),
                    },
                    |last| {
                        get_messages(u64::from_be_bytes(
                            // Safety: cannot fail since the remainder after we split is a valid u64
                            unsafe {
                                last.unwrap()
                                    .0
                                    .split_at(prefix.len())
                                    .1
                                    .try_into()
                                    .unwrap_unchecked()
                            },
                        ))
                    },
                )
            })
            .unwrap_or_else(|| get_messages(before_message))
    }

    pub fn get_user_roles_logic(&self, guild_id: u64, user_id: u64) -> Vec<u64> {
        let key = make_guild_user_roles_key(guild_id, user_id);
        cchat_get!(key).map_or_else(Vec::default, |raw| {
            raw.chunks_exact(size_of::<u64>())
                // Safety: this is safe since we split at u64 boundary
                .map(|raw| u64::from_be_bytes(unsafe { raw.try_into().unwrap_unchecked() }))
                .collect()
        })
    }

    pub fn query_has_permission_logic(
        &self,
        guild_id: u64,
        channel_id: u64,
        user_id: u64,
        check_for: &str,
    ) -> bool {
        let key = make_guild_user_roles_key(guild_id, user_id);
        let user_roles = self.get_list_u64_logic(&key);

        let is_ok = |key: &[u8]| {
            cchat_get!(key).map_or(false, |raw_perms| {
                let perms = db::deser_perm_list(raw_perms);
                perms
                    .permissions
                    .into_iter()
                    .filter(|perm| perm.matches.starts_with(check_for))
                    .any(|perm| matches!(perm.mode(), permission::Mode::Allow))
            })
        };

        if channel_id != 0 {
            for role_id in &user_roles {
                let key = &make_guild_channel_roles_key(guild_id, channel_id, *role_id);
                if is_ok(key) {
                    return true;
                }
            }
        }

        for role_id in user_roles {
            let key = &make_guild_role_perms_key(guild_id, role_id);
            if is_ok(key) {
                return true;
            }
        }

        false
    }

    pub fn check_perms(
        &self,
        guild_id: u64,
        channel_id: u64,
        user_id: u64,
        check_for: &str,
        must_be_guild_owner: bool,
    ) -> Result<(), <ChatServer as chat_service_server::ChatService>::Error> {
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
        cchat_remove!(make_member_key(guild_id, user_id));
        cchat_remove!(make_guild_user_roles_key(guild_id, user_id));
    }

    pub fn manage_user_roles_logic(
        &self,
        guild_id: u64,
        user_id: u64,
        give_role_ids: Vec<u64>,
        take_role_ids: Vec<u64>,
    ) -> Result<(), <ChatServer as chat_service_server::ChatService>::Error> {
        let mut roles = self.get_user_roles_logic(guild_id, user_id);
        for role_id in give_role_ids {
            if !self.does_role_exist(guild_id, role_id) {
                return Err(ServerError::NoSuchRole { guild_id, role_id });
            }
            roles.push(role_id);
        }
        for role_id in take_role_ids {
            if !self.does_role_exist(guild_id, role_id) {
                return Err(ServerError::NoSuchRole { guild_id, role_id });
            }
            if let Some(index) = roles.iter().position(|oid| role_id.eq(oid)) {
                roles.remove(index);
            }
        }

        let key = make_guild_user_roles_key(guild_id, user_id);
        let ser_roles = self.serialize_list_u64_logic(roles);
        cchat_insert!(key, ser_roles);

        Ok(())
    }

    pub fn add_default_role_to(
        &self,
        guild_id: u64,
        user_id: u64,
    ) -> Result<(), <ChatServer as chat_service_server::ChatService>::Error> {
        if let Some(raw) = cchat_get!(make_guild_default_role_key(guild_id)) {
            // Safety: safe since we only store valid u64 [ref:default_role_store]
            let default_role_id = u64::from_be_bytes(unsafe { raw.try_into().unwrap_unchecked() });
            self.manage_user_roles_logic(guild_id, user_id, vec![default_role_id], Vec::new())?;
        }
        Ok(())
    }

    pub fn add_guild_role_logic(
        &self,
        guild_id: u64,
        mut role: Role,
    ) -> Result<u64, <ChatServer as chat_service_server::ChatService>::Error> {
        let key = {
            role.role_id = gen_rand_u64();
            let mut key = make_guild_role_key(guild_id, role.role_id);
            while self.chat_tree.contains_key(&key).unwrap() {
                role.role_id = gen_rand_u64();
                key = make_guild_role_key(guild_id, role.role_id);
            }
            key
        };
        let role_id = role.role_id;
        let ser_role = encode_protobuf_message(role);
        cchat_insert!(key, ser_role);
        self.move_role_logic(guild_id, role_id, 0, 0)?;
        Ok(role_id)
    }

    pub fn set_permissions_logic(
        &self,
        guild_id: u64,
        channel_id: u64,
        role_id: u64,
        perms: PermissionList,
    ) {
        let put_perms = |key| {
            let buf = encode_protobuf_message(perms);
            cchat_insert!(key, buf);
        };
        // TODO: categories?
        if channel_id != 0 {
            put_perms(make_guild_channel_roles_key(guild_id, channel_id, role_id).as_ref())
        } else {
            put_perms(make_guild_role_perms_key(guild_id, role_id).as_ref())
        }
    }

    pub fn create_channel_logic(
        &self,
        guild_id: u64,
        channel_name: String,
        is_category: bool,
        metadata: Option<Metadata>,
        previous_id: u64,
        next_id: u64,
    ) -> Result<u64, <ChatServer as chat_service_server::ChatService>::Error> {
        let channel_id = {
            let mut channel_id = gen_rand_u64();
            let mut key = make_chan_key(guild_id, channel_id);
            while self.chat_tree.contains_key(&key).unwrap() {
                channel_id = gen_rand_u64();
                key = make_chan_key(guild_id, channel_id);
            }
            channel_id
        };
        let key = make_chan_key(guild_id, channel_id);

        let channel = Channel {
            metadata,
            channel_id,
            channel_name,
            is_category,
        };
        let buf = encode_protobuf_message(channel);
        cchat_insert!(key, buf);

        // Add from ordering list
        self.update_channel_order_logic(guild_id, channel_id, previous_id, next_id)?;

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
            .concat(),
            [],
        );
    }

    /// Removes a guild from a user's guild list
    pub fn remove_guild_from_guild_list(&self, user_id: u64, guild_id: u64, homeserver: &str) {
        cchat_remove!(make_guild_list_key(user_id, guild_id, homeserver));
    }

    /// Converts a local user ID to the corresponding foreign user ID and the host
    pub fn local_to_foreign_id(&self, local_id: u64) -> Option<(u64, SmolStr)> {
        let key = make_local_to_foreign_user_key(local_id);

        cchat_get!(key).map(|raw| {
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

        cchat_get!(key)
            // Safety: we store u64's only for these keys
            .map(|raw| u64::from_be_bytes(unsafe { raw.try_into().unwrap_unchecked() }))
    }
}
