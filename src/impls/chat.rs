use std::{collections::HashMap, convert::TryInto, sync::Arc};

use event::MessageUpdated;
use get_guild_channels_response::Channel;
use harmony_rust_sdk::api::{
    chat::*,
    exports::{
        hrpc::{encode_protobuf_message, Request},
        prost::{
            bytes::{Bytes, BytesMut},
            Message,
        },
    },
    harmonytypes::{Attachment, Message as HarmonyMessage},
};
use parking_lot::Mutex;
use sled::Tree;

use super::{gen_rand_str, gen_rand_u64};
use crate::{concat_static, ServerError};

fn make_msg_prefix(guild_id: u64, channel_id: u64) -> [u8; 18] {
    concat_static!(18, make_chan_key(guild_id, channel_id), [9])
}

fn make_msg_key(guild_id: u64, channel_id: u64, message_id: u64) -> [u8; 26] {
    concat_static!(
        26,
        make_msg_prefix(guild_id, channel_id),
        message_id.to_le_bytes()
    )
}

fn make_chan_key(guild_id: u64, channel_id: u64) -> [u8; 17] {
    concat_static!(
        17,
        make_guild_chan_prefix(guild_id),
        channel_id.to_le_bytes()
    )
}

fn make_guild_chan_prefix(guild_id: u64) -> [u8; 9] {
    concat_static!(9, guild_id.to_le_bytes(), [8])
}

fn make_member_key(guild_id: u64, user_id: u64) -> [u8; 17] {
    concat_static!(17, make_guild_mem_prefix(guild_id), user_id.to_le_bytes())
}

fn make_guild_mem_prefix(guild_id: u64) -> [u8; 9] {
    concat_static!(9, guild_id.to_le_bytes(), [9])
}

fn make_guild_list_key_prefix(user_id: u64) -> [u8; 10] {
    concat_static!(10, user_id.to_le_bytes(), [1, 2])
}

fn make_guild_list_key(user_id: u64, guild_id: u64, host: &str) -> Vec<u8> {
    [
        make_guild_list_key_prefix(user_id).as_ref(),
        guild_id.to_le_bytes().as_ref(),
        host.as_bytes(),
    ]
    .concat()
}

fn make_invite_key(name: &str) -> Vec<u8> {
    ["invite_".as_bytes(), name.as_bytes()].concat()
}

pub(super) fn make_member_profile_key(user_id: u64) -> [u8; 13] {
    concat_static!(13, "user_".as_bytes(), user_id.to_le_bytes())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EventSub {
    Guild(u64),
    Homeserver,
    Actions,
}

#[derive(Debug)]
pub struct ChatServer {
    valid_sessions: Arc<Mutex<HashMap<String, u64>>>,
    chat_tree: Tree,
    subbed_to: Mutex<HashMap<u64, Vec<EventSub>>>,
    event_chans: Mutex<HashMap<u64, Vec<event::Event>>>,
}

impl ChatServer {
    pub fn new(chat_tree: Tree, valid_sessions: Arc<Mutex<HashMap<String, u64>>>) -> Self {
        Self {
            valid_sessions,
            chat_tree,
            subbed_to: Mutex::new(HashMap::new()),
            event_chans: Mutex::new(HashMap::new()),
        }
    }

    fn send_event_through_chan(&self, sub: EventSub, event: event::Event) {
        for chan in self.event_chans.lock().values_mut() {
            for subbed_to in self.subbed_to.lock().values() {
                if subbed_to.contains(&sub) {
                    chan.push(event.clone());
                }
            }
        }
    }

    fn get_message(
        &self,
        guild_id: u64,
        channel_id: u64,
        message_id: u64,
    ) -> Result<(HarmonyMessage, [u8; 26]), <Self as chat_service_server::ChatService>::Error> {
        let key = make_msg_key(guild_id, channel_id, message_id);

        let message = if let Some(msg) = self.chat_tree.get(&key).unwrap() {
            HarmonyMessage::decode(Bytes::from(msg.to_vec())).unwrap()
        } else {
            return Err(ServerError::NoSuchMessage {
                guild_id,
                channel_id,
                message_id,
            });
        };

        Ok((message, key))
    }

    fn auth<T>(
        &self,
        request: &Request<T>,
    ) -> Result<u64, <Self as chat_service_server::ChatService>::Error> {
        let auth_id = request
            .get_header(&"Authorization".parse().unwrap())
            .map_or_else(String::default, |val| {
                val.to_str()
                    .map_or_else(|_| String::default(), ToString::to_string)
            });

        self.valid_sessions
            .lock()
            .get(&auth_id)
            .cloned()
            .map_or(Err(ServerError::Unauthenticated), Ok)
    }
}

#[harmony_rust_sdk::api::exports::hrpc::async_trait]
impl chat_service_server::ChatService for ChatServer {
    type Error = ServerError;

    async fn get_message(
        &self,
        request: Request<GetMessageRequest>,
    ) -> Result<GetMessageResponse, Self::Error> {
        self.auth(&request)?;

        let request = request.into_parts().0;

        let GetMessageRequest {
            guild_id,
            channel_id,
            message_id,
        } = request;

        let message = Some(self.get_message(guild_id, channel_id, message_id)?.0);

        Ok(GetMessageResponse { message })
    }

    async fn update_message(
        &self,
        request: Request<UpdateMessageRequest>,
    ) -> Result<(), Self::Error> {
        self.auth(&request)?;

        let request = request.into_parts().0;

        let UpdateMessageRequest {
            guild_id,
            channel_id,
            message_id,
            content,
            update_content,
            embeds,
            update_embeds,
            actions,
            update_actions,
            attachments,
            update_attachments,
            overrides,
            update_overrides,
            metadata,
            update_metadata,
        } = request;

        // TODO: process hmc here
        let attachments = attachments
            .into_iter()
            .map(|_hmc| Attachment {
                ..Default::default()
            })
            .collect::<Vec<_>>();

        let (mut message, key) = self.get_message(guild_id, channel_id, message_id)?;

        if update_content {
            message.content = content.clone();
        }
        if update_embeds {
            message.embeds = embeds.clone();
        }
        if update_actions {
            message.actions = actions.clone();
        }
        if update_attachments {
            message.attachments = attachments.clone();
        }
        if update_overrides {
            message.overrides = overrides.clone();
        }
        if update_metadata {
            message.metadata = metadata.clone();
        }

        let mut buf = Vec::with_capacity(message.encoded_len());
        // will never fail
        message.encode(&mut buf).unwrap();
        self.chat_tree.insert(&key, buf).unwrap();

        let edited_at = Some(std::time::SystemTime::now().into());

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::EditedMessage(Box::new(MessageUpdated {
                guild_id,
                channel_id,
                message_id,
                content,
                update_content,
                embeds,
                update_embeds,
                actions,
                update_actions,
                attachments,
                update_attachments,
                overrides,
                update_overrides,
                metadata,
                update_metadata,
                edited_at,
            })),
        );

        Ok(())
    }

    async fn create_guild(
        &self,
        request: Request<CreateGuildRequest>,
    ) -> Result<CreateGuildResponse, Self::Error> {
        let user_id = self.auth(&request)?;

        let (
            CreateGuildRequest {
                metadata,
                guild_name,
                picture_url,
            },
            headers,
        ) = request.into_parts();

        let guild_id = {
            let mut guild_id = gen_rand_u64();
            while self.chat_tree.contains_key(guild_id.to_le_bytes()).unwrap() {
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
        let mut buf = BytesMut::new();
        encode_protobuf_message(&mut buf, guild);

        self.chat_tree
            .insert(guild_id.to_le_bytes().as_ref(), buf.as_ref())
            .unwrap();

        self.add_guild_to_guild_list(Request::from_parts((
            AddGuildToGuildListRequest {
                guild_id,
                homeserver: "".to_string(),
            },
            headers,
        )))
        .await?;

        self.chat_tree
            .insert(&make_member_key(guild_id, user_id), &[])
            .unwrap();

        Ok(CreateGuildResponse { guild_id })
    }

    async fn create_invite(
        &self,
        request: Request<CreateInviteRequest>,
    ) -> Result<CreateInviteResponse, Self::Error> {
        self.auth(&request)?;

        let CreateInviteRequest {
            guild_id,
            name,
            possible_uses,
        } = request.into_parts().0;

        let key = make_invite_key(name.as_str());

        let invite = get_guild_invites_response::Invite {
            possible_uses,
            use_count: 0,
            invite_id: name.clone(),
        };
        let mut buf = BytesMut::new();
        encode_protobuf_message(&mut buf, invite);

        self.chat_tree
            .insert(
                key,
                [guild_id.to_le_bytes().as_ref(), buf.as_ref()].concat(),
            )
            .unwrap();

        Ok(CreateInviteResponse { name })
    }

    async fn create_channel(
        &self,
        request: Request<CreateChannelRequest>,
    ) -> Result<CreateChannelResponse, Self::Error> {
        self.auth(&request)?;

        // TODO: do ordering
        let CreateChannelRequest {
            guild_id,
            channel_name,
            is_category,
            previous_id,
            next_id,
            metadata,
        } = request.into_parts().0;

        let channel_id = {
            let mut channel_id = gen_rand_u64();
            let mut key = make_chan_key(guild_id, channel_id);
            while self.chat_tree.contains_key(key).unwrap() {
                channel_id = gen_rand_u64();
                key = make_chan_key(guild_id, channel_id);
            }
            channel_id
        };
        let key = make_chan_key(guild_id, channel_id);

        let channel = Channel {
            channel_id,
            channel_name: channel_name.clone(),
            is_category,
            metadata: metadata.clone(),
        };
        let mut buf = BytesMut::new();
        encode_protobuf_message(&mut buf, channel);

        self.chat_tree.insert(key.as_ref(), buf.as_ref()).unwrap();

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
        );

        Ok(CreateChannelResponse { channel_id })
    }

    async fn create_emote_pack(
        &self,
        request: Request<CreateEmotePackRequest>,
    ) -> Result<CreateEmotePackResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn get_guild_list(
        &self,
        request: Request<GetGuildListRequest>,
    ) -> Result<GetGuildListResponse, Self::Error> {
        let user_id = self.auth(&request)?;

        let guilds = self
            .chat_tree
            .scan_prefix(make_guild_list_key_prefix(user_id))
            .flatten()
            .map(|(_, guild_id_raw)| {
                let (id_raw, host_raw) = guild_id_raw.split_at(std::mem::size_of::<u64>());

                let guild_id = u64::from_le_bytes(id_raw.try_into().unwrap());
                let host = std::str::from_utf8(host_raw).unwrap();

                get_guild_list_response::GuildListEntry {
                    guild_id,
                    host: host.to_string(),
                }
            })
            .collect();

        Ok(GetGuildListResponse { guilds })
    }

    async fn add_guild_to_guild_list(
        &self,
        request: Request<AddGuildToGuildListRequest>,
    ) -> Result<AddGuildToGuildListResponse, Self::Error> {
        let user_id = self.auth(&request)?;

        let AddGuildToGuildListRequest {
            guild_id,
            homeserver,
        } = request.into_parts().0;

        let serialized = [
            guild_id.to_le_bytes().as_ref(),
            homeserver.as_str().as_bytes(),
        ]
        .concat();

        self.chat_tree
            .insert(
                [
                    make_guild_list_key_prefix(user_id).as_ref(),
                    serialized.as_slice(),
                ]
                .concat(),
                serialized,
            )
            .unwrap();

        self.send_event_through_chan(
            EventSub::Homeserver,
            event::Event::GuildAddedToList(event::GuildAddedToList {
                guild_id,
                homeserver,
            }),
        );

        Ok(AddGuildToGuildListResponse {})
    }

    async fn remove_guild_from_guild_list(
        &self,
        request: Request<RemoveGuildFromGuildListRequest>,
    ) -> Result<RemoveGuildFromGuildListResponse, Self::Error> {
        let user_id = self.auth(&request)?;

        let RemoveGuildFromGuildListRequest {
            guild_id,
            homeserver,
        } = request.into_parts().0;

        self.chat_tree
            .remove(make_guild_list_key(user_id, guild_id, homeserver.as_str()))
            .unwrap();

        self.send_event_through_chan(
            EventSub::Homeserver,
            event::Event::GuildRemovedFromList(event::GuildRemovedFromList {
                guild_id,
                homeserver,
            }),
        );

        Ok(RemoveGuildFromGuildListResponse {})
    }

    async fn get_guild(
        &self,
        request: Request<GetGuildRequest>,
    ) -> Result<GetGuildResponse, Self::Error> {
        self.auth(&request)?;

        let GetGuildRequest { guild_id } = request.into_parts().0;

        let guild =
            if let Some(guild_raw) = self.chat_tree.get(guild_id.to_le_bytes().as_ref()).unwrap() {
                GetGuildResponse::decode(guild_raw.as_ref()).unwrap()
            } else {
                return Err(ServerError::NoSuchGuild(guild_id));
            };

        Ok(guild)
    }

    async fn get_guild_invites(
        &self,
        request: Request<GetGuildInvitesRequest>,
    ) -> Result<GetGuildInvitesResponse, Self::Error> {
        self.auth(&request)?;

        let GetGuildInvitesRequest { guild_id } = request.into_parts().0;

        let invites = self
            .chat_tree
            .scan_prefix("invite_")
            .flatten()
            .map(|(_, value)| {
                let (id_raw, invite_raw) = value.split_at(std::mem::size_of::<u64>());
                let id = u64::from_le_bytes(id_raw.try_into().unwrap());
                let invite = get_guild_invites_response::Invite::decode(invite_raw).unwrap();
                (id, invite)
            })
            .filter_map(|(id, invite)| if guild_id == id { Some(invite) } else { None })
            .collect();

        Ok(GetGuildInvitesResponse { invites })
    }

    async fn get_guild_members(
        &self,
        request: Request<GetGuildMembersRequest>,
    ) -> Result<GetGuildMembersResponse, Self::Error> {
        self.auth(&request)?;

        let GetGuildMembersRequest { guild_id } = request.into_parts().0;

        let prefix = make_guild_mem_prefix(guild_id);
        let members = self
            .chat_tree
            .scan_prefix(prefix)
            .flatten()
            .map(|(id, _)| u64::from_le_bytes(id.split_at(prefix.len()).1.try_into().unwrap()))
            .collect();

        Ok(GetGuildMembersResponse { members })
    }

    // TODO: do ordering
    async fn get_guild_channels(
        &self,
        request: Request<GetGuildChannelsRequest>,
    ) -> Result<GetGuildChannelsResponse, Self::Error> {
        self.auth(&request)?;

        let GetGuildChannelsRequest { guild_id } = request.into_parts().0;

        let prefix = make_guild_chan_prefix(guild_id);
        let channels = self
            .chat_tree
            .scan_prefix(prefix)
            .flatten()
            .flat_map(|(key, value)| {
                if key.len() == 17 {
                    Some(Channel::decode(value.as_ref()).unwrap())
                } else {
                    None
                }
            })
            .collect();

        Ok(GetGuildChannelsResponse { channels })
    }

    async fn get_channel_messages(
        &self,
        request: Request<GetChannelMessagesRequest>,
    ) -> Result<GetChannelMessagesResponse, Self::Error> {
        self.auth(&request)?;

        let GetChannelMessagesRequest {
            guild_id,
            channel_id,
            before_message,
        } = request.into_parts().0;

        let prefix = make_msg_prefix(guild_id, channel_id);
        let mut msgs = self
            .chat_tree
            .scan_prefix(prefix)
            .flatten()
            .map(|(key, value)| {
                (
                    u64::from_le_bytes(key.split_at(prefix.len()).1.try_into().unwrap()),
                    value,
                )
            })
            .collect::<Vec<_>>();

        let before_msg_pos = msgs
            .iter()
            .position(|(id, _)| *id == before_message)
            .unwrap_or_else(|| msgs.len());

        let from = before_msg_pos.saturating_sub(25);
        let to = before_msg_pos;

        Ok(GetChannelMessagesResponse {
            reached_top: from == 0,
            messages: msgs
                .drain(from..to)
                .map(|(_, msg_raw)| HarmonyMessage::decode(msg_raw.as_ref()).unwrap())
                .rev()
                .collect(),
        })
    }

    async fn get_emote_packs(
        &self,
        request: Request<GetEmotePacksRequest>,
    ) -> Result<GetEmotePacksResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn get_emote_pack_emotes(
        &self,
        request: Request<GetEmotePackEmotesRequest>,
    ) -> Result<GetEmotePackEmotesResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn update_guild_information(
        &self,
        request: Request<UpdateGuildInformationRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn update_channel_information(
        &self,
        request: Request<UpdateChannelInformationRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn update_channel_order(
        &self,
        request: Request<UpdateChannelOrderRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn add_emote_to_pack(
        &self,
        request: Request<AddEmoteToPackRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn delete_guild(&self, request: Request<DeleteGuildRequest>) -> Result<(), Self::Error> {
        self.auth(&request)?;

        let DeleteGuildRequest { guild_id } = request.into_parts().0;

        let chan_keys = self
            .chat_tree
            .scan_prefix(make_guild_chan_prefix(guild_id))
            .flatten()
            .map(|(key, _)| key);
        let members = self
            .chat_tree
            .scan_prefix(make_guild_mem_prefix(guild_id))
            .flatten()
            .map(|(key, _)| key);

        let mut batch = sled::Batch::default();
        for key in chan_keys {
            batch.remove(&key);
        }
        for member in members {
            batch.remove(&member);
        }
        self.chat_tree.apply_batch(batch).unwrap();

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::DeletedGuild(event::GuildDeleted { guild_id }),
        );

        Ok(())
    }

    async fn delete_invite(
        &self,
        request: Request<DeleteInviteRequest>,
    ) -> Result<(), Self::Error> {
        self.auth(&request)?;

        let DeleteInviteRequest {
            guild_id: _,
            invite_id,
        } = request.into_parts().0;

        self.chat_tree
            .remove(make_invite_key(invite_id.as_str()))
            .unwrap();

        Ok(())
    }

    async fn delete_channel(
        &self,
        request: Request<DeleteChannelRequest>,
    ) -> Result<(), Self::Error> {
        self.auth(&request)?;

        let DeleteChannelRequest {
            guild_id,
            channel_id,
        } = request.into_parts().0;

        let messages = self
            .chat_tree
            .scan_prefix(make_msg_prefix(guild_id, channel_id))
            .flatten()
            .map(|(k, _)| k);

        let mut batch = sled::Batch::default();
        batch.remove(&make_chan_key(guild_id, channel_id));
        for key in messages {
            batch.remove(key);
        }
        self.chat_tree.apply_batch(batch).unwrap();

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::DeletedChannel(event::ChannelDeleted {
                guild_id,
                channel_id,
            }),
        );

        Ok(())
    }

    async fn delete_message(
        &self,
        request: Request<DeleteMessageRequest>,
    ) -> Result<(), Self::Error> {
        self.auth(&request)?;

        let DeleteMessageRequest {
            guild_id,
            channel_id,
            message_id,
        } = request.into_parts().0;

        self.chat_tree
            .remove(make_msg_key(guild_id, channel_id, message_id))
            .unwrap();

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::DeletedMessage(event::MessageDeleted {
                channel_id,
                guild_id,
                message_id,
            }),
        );

        Ok(())
    }

    async fn delete_emote_from_pack(
        &self,
        request: Request<DeleteEmoteFromPackRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn delete_emote_pack(
        &self,
        request: Request<DeleteEmotePackRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn dequip_emote_pack(
        &self,
        request: Request<DequipEmotePackRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn join_guild(
        &self,
        request: Request<JoinGuildRequest>,
    ) -> Result<JoinGuildResponse, Self::Error> {
        let user_id = self.auth(&request)?;

        let (JoinGuildRequest { invite_id }, headers) = request.into_parts();
        let key = make_invite_key(invite_id.as_str());

        let (guild_id, mut invite) = if let Some(raw) = self.chat_tree.get(&key).unwrap() {
            let (id_raw, invite_raw) = raw.split_at(std::mem::size_of::<u64>());
            let guild_id = u64::from_le_bytes(id_raw.try_into().unwrap());
            let invite = get_guild_invites_response::Invite::decode(invite_raw).unwrap();
            (guild_id, invite)
        } else {
            return Err(ServerError::NoSuchInvite(invite_id));
        };

        if invite.use_count < invite.possible_uses || invite.possible_uses == -1 {
            self.chat_tree
                .insert(&make_member_key(guild_id, user_id), &[])
                .unwrap();
            invite.use_count += 1;

            self.add_guild_to_guild_list(Request::from_parts((
                AddGuildToGuildListRequest {
                    guild_id,
                    homeserver: "".to_string(),
                },
                headers,
            )))
            .await?;

            self.send_event_through_chan(
                EventSub::Guild(guild_id),
                event::Event::JoinedMember(event::MemberJoined {
                    guild_id,
                    member_id: user_id,
                }),
            );

            let mut buf = BytesMut::new();
            encode_protobuf_message(&mut buf, invite);
            self.chat_tree
                .insert(
                    &key,
                    [guild_id.to_le_bytes().as_ref(), buf.as_ref()].concat(),
                )
                .unwrap();
        }

        Ok(JoinGuildResponse { guild_id })
    }

    async fn leave_guild(&self, request: Request<LeaveGuildRequest>) -> Result<(), Self::Error> {
        let user_id = self.auth(&request)?;

        let (LeaveGuildRequest { guild_id }, headers) = request.into_parts();

        self.chat_tree
            .remove(&make_member_key(guild_id, user_id))
            .unwrap();

        self.remove_guild_from_guild_list(Request::from_parts((
            RemoveGuildFromGuildListRequest {
                guild_id,
                homeserver: "".to_string(),
            },
            headers,
        )))
        .await?;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::LeftMember(event::MemberLeft {
                guild_id,
                member_id: user_id,
                leave_reason: 0,
            }),
        );

        Ok(())
    }

    async fn trigger_action(
        &self,
        request: Request<TriggerActionRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<SendMessageResponse, Self::Error> {
        let user_id = self.auth(&request)?;

        let request = request.into_parts().0;

        let SendMessageRequest {
            guild_id,
            channel_id,
            content,
            actions,
            embeds,
            attachments,
            in_reply_to,
            overrides,
            echo_id,
            metadata,
        } = request;

        let (message_id, key) = {
            let mut message_id = gen_rand_u64();
            let mut key = make_msg_key(guild_id, channel_id, message_id);
            while self.chat_tree.contains_key(key).unwrap() {
                message_id = gen_rand_u64();
                key = make_msg_key(guild_id, channel_id, message_id);
            }
            (message_id, key)
        };

        // TODO: process hmc here
        let attachments = attachments
            .into_iter()
            .map(|_hmc| Attachment {
                ..Default::default()
            })
            .collect::<Vec<_>>();

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
            embeds,
            actions,
            attachments,
            in_reply_to,
            overrides,
        };

        let mut buf = Vec::with_capacity(message.encoded_len());
        // will never fail
        message.encode(&mut buf).unwrap();
        self.chat_tree.insert(&key, buf).unwrap();

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::SentMessage(Box::new(event::MessageSent {
                echo_id,
                message: Some(message),
            })),
        );

        Ok(SendMessageResponse { message_id })
    }

    async fn query_has_permission(
        &self,
        request: Request<QueryPermissionsRequest>,
    ) -> Result<QueryPermissionsResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn set_permissions(
        &self,
        request: Request<SetPermissionsRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn get_permissions(
        &self,
        request: Request<GetPermissionsRequest>,
    ) -> Result<GetPermissionsResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn move_role(
        &self,
        request: Request<MoveRoleRequest>,
    ) -> Result<MoveRoleResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn get_guild_roles(
        &self,
        request: Request<GetGuildRolesRequest>,
    ) -> Result<GetGuildRolesResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn add_guild_role(
        &self,
        request: Request<AddGuildRoleRequest>,
    ) -> Result<AddGuildRoleResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn modify_guild_role(
        &self,
        request: Request<ModifyGuildRoleRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn delete_guild_role(
        &self,
        request: Request<DeleteGuildRoleRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn manage_user_roles(
        &self,
        request: Request<ManageUserRolesRequest>,
    ) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn get_user_roles(
        &self,
        request: Request<GetUserRolesRequest>,
    ) -> Result<GetUserRolesResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn stream_events(
        &self,
        validation_request: &Request<()>,
        request: Option<StreamEventsRequest>,
    ) -> Result<Option<Event>, Self::Error> {
        let user_id = self.auth(validation_request)?;

        if let Some(req) = request.map(|r| r.request).flatten() {
            use stream_events_request::*;

            let sub = match req {
                Request::SubscribeToGuild(SubscribeToGuild { guild_id }) => {
                    EventSub::Guild(guild_id)
                }
                Request::SubscribeToActions(SubscribeToActions {}) => EventSub::Actions,
                Request::SubscribeToHomeserverEvents(SubscribeToHomeserverEvents {}) => {
                    EventSub::Homeserver
                }
            };

            self.subbed_to.lock().entry(user_id).or_default().push(sub);
        }

        let event = self
            .event_chans
            .lock()
            .entry(user_id)
            .or_default()
            .pop()
            .map(|event| Event { event: Some(event) });

        Ok(event)
    }

    async fn sync(
        &self,
        validation_request: &Request<SyncRequest>,
    ) -> Result<Option<SyncEvent>, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn get_user(
        &self,
        request: Request<GetUserRequest>,
    ) -> Result<GetUserResponse, Self::Error> {
        self.auth(&request)?;

        let GetUserRequest { user_id } = request.into_parts().0;

        let key = make_member_profile_key(user_id);

        let profile = if let Some(profile_raw) = self.chat_tree.get(key).unwrap() {
            GetUserResponse::decode(profile_raw.as_ref()).unwrap()
        } else {
            return Err(ServerError::NoSuchUser(user_id));
        };

        Ok(profile)
    }

    async fn get_user_metadata(
        &self,
        request: Request<GetUserMetadataRequest>,
    ) -> Result<GetUserMetadataResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn profile_update(
        &self,
        request: Request<ProfileUpdateRequest>,
    ) -> Result<(), Self::Error> {
        let user_id = self.auth(&request)?;

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

        let key = make_member_profile_key(user_id);

        let mut profile = self
            .chat_tree
            .get(key)
            .unwrap()
            .map_or_else(GetUserResponse::default, |e| {
                GetUserResponse::decode(e.as_ref()).unwrap()
            });

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

        let mut buf = BytesMut::new();
        encode_protobuf_message(&mut buf, profile);
        self.chat_tree.insert(key, buf.as_ref()).unwrap();

        self.send_event_through_chan(
            EventSub::Homeserver,
            event::Event::ProfileUpdated(event::ProfileUpdated {
                update_avatar,
                update_is_bot,
                update_status,
                update_username,
                new_avatar,
                new_status,
                new_username,
                is_bot,
                user_id,
            }),
        );

        Ok(())
    }

    async fn typing(&self, request: Request<TypingRequest>) -> Result<(), Self::Error> {
        let user_id = self.auth(&request)?;

        let TypingRequest {
            guild_id,
            channel_id,
        } = request.into_parts().0;

        self.send_event_through_chan(
            EventSub::Guild(guild_id),
            event::Event::Typing(event::Typing {
                channel_id,
                guild_id,
                user_id,
            }),
        );

        Ok(())
    }

    async fn preview_guild(
        &self,
        request: Request<PreviewGuildRequest>,
    ) -> Result<PreviewGuildResponse, Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn ban_user(&self, request: Request<BanUserRequest>) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn kick_user(&self, request: Request<KickUserRequest>) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }

    async fn unban_user(&self, request: Request<UnbanUserRequest>) -> Result<(), Self::Error> {
        Err(ServerError::NotImplemented)
    }
}
