use std::{collections::HashMap, convert::TryInto};

use event::MessageUpdated;
use harmony_rust_sdk::api::{
    chat::*,
    exports::prost::{bytes::Bytes, Message},
    harmonytypes::{Attachment, Message as HarmonyMessage},
};
use parking_lot::Mutex;
use sled::Db;

use super::{gen_rand_str, gen_rand_u64};
use crate::{concat_static, ServerError};

fn make_msg_key(guild_id: u64, channel_id: u64, message_id: u64) -> [u8; 24] {
    concat_static!(
        24,
        guild_id.to_le_bytes(),
        channel_id.to_le_bytes(),
        message_id.to_le_bytes()
    )
}

#[derive(Debug)]
pub struct ChatServer {
    db: Db,
    event_chans: Vec<flume::Sender<event::Event>>,
}

impl ChatServer {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            event_chans: Vec::new(),
        }
    }

    fn get_message(
        &self,
        guild_id: u64,
        channel_id: u64,
        message_id: u64,
    ) -> (HarmonyMessage, [u8; 24]) {
        let chat_tree = self.db.open_tree("chat").unwrap();

        let key = make_msg_key(guild_id, channel_id, message_id);

        let message = if let Some(msg) = chat_tree.get(&key).unwrap() {
            match HarmonyMessage::decode(Bytes::from(msg.to_vec())) {
                Ok(m) => m,
                Err(_) => todo!("return failed to decode msg internal server error"),
            }
        } else {
            todo!("return no such message error")
        };

        (message, key)
    }
}

#[harmony_rust_sdk::api::exports::hrpc::async_trait]
impl chat_service_server::ChatService for ChatServer {
    type Error = ServerError;

    async fn create_guild(
        &self,
        request: CreateGuildRequest,
    ) -> Result<CreateGuildResponse, Self::Error> {
        todo!()
    }

    async fn create_invite(
        &self,
        request: CreateInviteRequest,
    ) -> Result<CreateInviteResponse, Self::Error> {
        todo!()
    }

    async fn create_channel(
        &self,
        request: CreateChannelRequest,
    ) -> Result<CreateChannelResponse, Self::Error> {
        todo!()
    }

    async fn create_emote_pack(
        &self,
        request: CreateEmotePackRequest,
    ) -> Result<CreateEmotePackResponse, Self::Error> {
        todo!()
    }

    async fn get_guild_list(
        &self,
        request: GetGuildListRequest,
    ) -> Result<GetGuildListResponse, Self::Error> {
        todo!()
    }

    async fn add_guild_to_guild_list(
        &self,
        request: AddGuildToGuildListRequest,
    ) -> Result<AddGuildToGuildListResponse, Self::Error> {
        todo!()
    }

    async fn remove_guild_from_guild_list(
        &self,
        request: RemoveGuildFromGuildListRequest,
    ) -> Result<RemoveGuildFromGuildListResponse, Self::Error> {
        todo!()
    }

    async fn get_guild(&self, request: GetGuildRequest) -> Result<GetGuildResponse, Self::Error> {
        todo!()
    }

    async fn get_guild_invites(
        &self,
        request: GetGuildInvitesRequest,
    ) -> Result<GetGuildInvitesResponse, Self::Error> {
        todo!()
    }

    async fn get_guild_members(
        &self,
        request: GetGuildMembersRequest,
    ) -> Result<GetGuildMembersResponse, Self::Error> {
        todo!()
    }

    async fn get_guild_channels(
        &self,
        request: GetGuildChannelsRequest,
    ) -> Result<GetGuildChannelsResponse, Self::Error> {
        todo!()
    }

    async fn get_channel_messages(
        &self,
        request: GetChannelMessagesRequest,
    ) -> Result<GetChannelMessagesResponse, Self::Error> {
        todo!()
    }

    async fn get_message(
        &self,
        request: GetMessageRequest,
    ) -> Result<GetMessageResponse, Self::Error> {
        let GetMessageRequest {
            guild_id,
            channel_id,
            message_id,
        } = request;

        let message = Some(self.get_message(guild_id, channel_id, message_id).0);

        Ok(GetMessageResponse { message })
    }

    async fn get_emote_packs(
        &self,
        request: GetEmotePacksRequest,
    ) -> Result<GetEmotePacksResponse, Self::Error> {
        todo!()
    }

    async fn get_emote_pack_emotes(
        &self,
        request: GetEmotePackEmotesRequest,
    ) -> Result<GetEmotePackEmotesResponse, Self::Error> {
        todo!()
    }

    async fn update_guild_information(
        &self,
        request: UpdateGuildInformationRequest,
    ) -> Result<(), Self::Error> {
        todo!()
    }

    async fn update_channel_information(
        &self,
        request: UpdateChannelInformationRequest,
    ) -> Result<(), Self::Error> {
        todo!()
    }

    async fn update_channel_order(
        &self,
        request: UpdateChannelOrderRequest,
    ) -> Result<(), Self::Error> {
        todo!()
    }

    async fn update_message(&self, request: UpdateMessageRequest) -> Result<(), Self::Error> {
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

        let chat_tree = self.db.open_tree("chat").unwrap();

        let key = make_msg_key(guild_id, channel_id, message_id);

        let mut message = if let Some(msg) = chat_tree.get(&key).unwrap() {
            match HarmonyMessage::decode(Bytes::from(msg.to_vec())) {
                Ok(m) => m,
                Err(_) => todo!("return failed to decode msg internal server error"),
            }
        } else {
            todo!("return no such message error")
        };

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
        chat_tree.insert(&key, buf).unwrap();

        let edited_at = Some(std::time::SystemTime::now().into());

        for chan in &self.event_chans {
            chan.send_async(event::Event::EditedMessage(Box::new(MessageUpdated {
                guild_id,
                channel_id,
                message_id,
                content: content.clone(),
                update_content,
                embeds: embeds.clone(),
                update_embeds,
                actions: actions.clone(),
                update_actions,
                attachments: attachments.clone(),
                update_attachments,
                overrides: overrides.clone(),
                update_overrides,
                metadata: metadata.clone(),
                update_metadata,
                edited_at: edited_at.clone(),
            })))
            .await
            .unwrap();
        }

        Ok(())
    }

    async fn add_emote_to_pack(&self, request: AddEmoteToPackRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn delete_guild(&self, request: DeleteGuildRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn delete_invite(&self, request: DeleteInviteRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn delete_channel(&self, request: DeleteChannelRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn delete_message(&self, request: DeleteMessageRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn delete_emote_from_pack(
        &self,
        request: DeleteEmoteFromPackRequest,
    ) -> Result<(), Self::Error> {
        todo!()
    }

    async fn delete_emote_pack(&self, request: DeleteEmotePackRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn dequip_emote_pack(&self, request: DequipEmotePackRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn join_guild(
        &self,
        request: JoinGuildRequest,
    ) -> Result<JoinGuildResponse, Self::Error> {
        todo!()
    }

    async fn leave_guild(&self, request: LeaveGuildRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn trigger_action(&self, request: TriggerActionRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn send_message(
        &self,
        request: SendMessageRequest,
    ) -> Result<SendMessageResponse, Self::Error> {
        todo!()
    }

    async fn query_has_permission(
        &self,
        request: QueryPermissionsRequest,
    ) -> Result<QueryPermissionsResponse, Self::Error> {
        todo!()
    }

    async fn set_permissions(&self, request: SetPermissionsRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn get_permissions(
        &self,
        request: GetPermissionsRequest,
    ) -> Result<GetPermissionsResponse, Self::Error> {
        todo!()
    }

    async fn move_role(&self, request: MoveRoleRequest) -> Result<MoveRoleResponse, Self::Error> {
        todo!()
    }

    async fn get_guild_roles(
        &self,
        request: GetGuildRolesRequest,
    ) -> Result<GetGuildRolesResponse, Self::Error> {
        todo!()
    }

    async fn add_guild_role(
        &self,
        request: AddGuildRoleRequest,
    ) -> Result<AddGuildRoleResponse, Self::Error> {
        todo!()
    }

    async fn modify_guild_role(&self, request: ModifyGuildRoleRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn delete_guild_role(&self, request: DeleteGuildRoleRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn manage_user_roles(&self, request: ManageUserRolesRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn get_user_roles(
        &self,
        request: GetUserRolesRequest,
    ) -> Result<GetUserRolesResponse, Self::Error> {
        todo!()
    }

    async fn stream_events(&self, request: StreamEventsRequest) -> Result<Event, Self::Error> {
        todo!()
    }

    async fn get_user(&self, request: GetUserRequest) -> Result<GetUserResponse, Self::Error> {
        todo!()
    }

    async fn get_user_metadata(
        &self,
        request: GetUserMetadataRequest,
    ) -> Result<GetUserMetadataResponse, Self::Error> {
        todo!()
    }

    async fn profile_update(&self, request: ProfileUpdateRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn typing(&self, request: TypingRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn preview_guild(
        &self,
        request: PreviewGuildRequest,
    ) -> Result<PreviewGuildResponse, Self::Error> {
        todo!()
    }

    async fn ban_user(&self, request: BanUserRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn kick_user(&self, request: KickUserRequest) -> Result<(), Self::Error> {
        todo!()
    }

    async fn unban_user(&self, request: UnbanUserRequest) -> Result<(), Self::Error> {
        todo!()
    }
}
