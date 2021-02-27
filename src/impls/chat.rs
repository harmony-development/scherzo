use std::{collections::HashMap, convert::TryInto};

use harmony_rust_sdk::api::chat::*;
use parking_lot::Mutex;
use sled::Db;

use super::{gen_rand_str, gen_rand_u64};
use crate::ServerError;

#[derive(Debug)]
pub struct ChatServer {
    db: Db,
}

impl ChatServer {
    pub fn new(db: Db) -> Self {
        Self { db }
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
        todo!()
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
        todo!()
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
