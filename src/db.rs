use std::mem::size_of;

use cached::proc_macro::cached;
use harmony_rust_sdk::api::{
    chat::{
        get_guild_channels_response::Channel, get_guild_invites_response::Invite, GetGuildResponse,
        GetUserResponse, PermissionList, Role,
    },
    exports::prost::Message,
    harmonytypes::Message as HarmonyMessage,
};

pub mod chat {
    use super::concat_static;

    pub const USER_PREFIX: &[u8] = b"user_";
    pub const FOREIGN_PREFIX: &[u8] = b"fuser_";
    pub const INVITE_PREFIX: &[u8] = b"invite_";

    // message

    pub const fn make_msg_prefix(guild_id: u64, channel_id: u64) -> [u8; 18] {
        concat_static(&[&make_chan_key(guild_id, channel_id), &[9]])
    }

    pub const fn make_msg_key(guild_id: u64, channel_id: u64, message_id: u64) -> [u8; 26] {
        concat_static(&[
            &make_msg_prefix(guild_id, channel_id),
            &message_id.to_be_bytes(),
        ])
    }

    // message

    // member

    pub const fn make_member_key(guild_id: u64, user_id: u64) -> [u8; 17] {
        concat_static(&[&make_guild_mem_prefix(guild_id), &user_id.to_be_bytes()])
    }

    pub const fn make_banned_member_key(guild_id: u64, user_id: u64) -> [u8; 17] {
        concat_static(&[
            &make_guild_banned_mem_prefix(guild_id),
            &user_id.to_be_bytes(),
        ])
    }

    pub const fn make_local_to_foreign_user_key(local_id: u64) -> [u8; 14] {
        concat_static(&[FOREIGN_PREFIX, &local_id.to_be_bytes(), &[2]])
    }

    pub fn make_foreign_to_local_user_key(foreign_id: u64, host: &str) -> Vec<u8> {
        [
            FOREIGN_PREFIX,
            &[2],
            &foreign_id.to_be_bytes(),
            host.as_bytes(),
        ]
        .concat()
    }

    pub const fn make_user_profile_key(user_id: u64) -> [u8; 13] {
        concat_static(&[USER_PREFIX, &user_id.to_be_bytes()])
    }

    pub fn make_user_metadata_key(user_id: u64, app_id: &str) -> Vec<u8> {
        [
            make_user_profile_key(user_id).as_ref(),
            &[1],
            app_id.as_bytes(),
        ]
        .concat()
    }

    // member

    // guild

    pub const fn make_guild_mem_prefix(guild_id: u64) -> [u8; 9] {
        concat_static(&[&guild_id.to_be_bytes(), &[9]])
    }

    pub const fn make_guild_chan_prefix(guild_id: u64) -> [u8; 9] {
        concat_static(&[&guild_id.to_be_bytes(), &[8]])
    }

    pub const fn make_guild_banned_mem_prefix(guild_id: u64) -> [u8; 9] {
        concat_static(&[&guild_id.to_be_bytes(), &[7]])
    }

    pub const fn make_guild_role_prefix(guild_id: u64) -> [u8; 9] {
        concat_static(&[&guild_id.to_be_bytes(), &[5]])
    }

    pub const fn make_guild_user_roles_prefix(guild_id: u64) -> [u8; 9] {
        concat_static(&[&guild_id.to_be_bytes(), &[4]])
    }

    pub const fn make_guild_default_role_key(guild_id: u64) -> [u8; 10] {
        concat_static(&[&guild_id.to_be_bytes(), &[1, 4]])
    }

    pub const fn make_guild_role_ordering_key(guild_id: u64) -> [u8; 10] {
        concat_static(&[&guild_id.to_be_bytes(), &[1, 3]])
    }

    pub const fn make_guild_list_key_prefix(user_id: u64) -> [u8; 10] {
        concat_static(&[&user_id.to_be_bytes(), &[1, 2]])
    }

    pub const fn make_guild_chan_ordering_key(guild_id: u64) -> [u8; 10] {
        concat_static(&[&guild_id.to_be_bytes(), &[1, 1]])
    }

    pub fn make_guild_list_key(user_id: u64, guild_id: u64, host: &str) -> Vec<u8> {
        [
            make_guild_list_key_prefix(user_id).as_ref(),
            guild_id.to_be_bytes().as_ref(),
            host.as_bytes(),
        ]
        .concat()
    }

    pub const fn make_guild_role_key(guild_id: u64, role_id: u64) -> [u8; 17] {
        concat_static(&[&make_guild_role_prefix(guild_id), &role_id.to_be_bytes()])
    }

    pub const fn make_guild_role_perms_key(guild_id: u64, role_id: u64) -> [u8; 18] {
        concat_static(&[
            &make_guild_role_prefix(guild_id),
            &role_id.to_be_bytes(),
            &[9],
        ])
    }

    pub const fn make_guild_user_roles_key(guild_id: u64, user_id: u64) -> [u8; 17] {
        concat_static(&[
            &make_guild_user_roles_prefix(guild_id),
            &user_id.to_be_bytes(),
        ])
    }

    pub const fn make_guild_channel_roles_key(
        guild_id: u64,
        channel_id: u64,
        role_id: u64,
    ) -> [u8; 26] {
        concat_static(&[
            &make_chan_key(guild_id, channel_id),
            &[8],
            &role_id.to_be_bytes(),
        ])
    }

    // guild

    pub const fn make_chan_key(guild_id: u64, channel_id: u64) -> [u8; 17] {
        concat_static(&[&make_guild_chan_prefix(guild_id), &channel_id.to_be_bytes()])
    }

    pub fn make_invite_key(name: &str) -> Vec<u8> {
        [INVITE_PREFIX, name.as_bytes()].concat()
    }
}

pub mod auth {
    use super::concat_static;

    pub const ATIME_PREFIX: &[u8] = b"atime_";
    pub const TOKEN_PREFIX: &[u8] = b"token_";

    pub const fn token_key(user_id: u64) -> [u8; 14] {
        concat_static(&[TOKEN_PREFIX, &user_id.to_be_bytes()])
    }

    pub const fn atime_key(user_id: u64) -> [u8; 14] {
        concat_static(&[ATIME_PREFIX, &user_id.to_be_bytes()])
    }
}

pub mod sync {
    pub const HOST_PREFIX: &[u8] = b"host_";

    pub fn make_host_key(host: &str) -> Vec<u8> {
        [HOST_PREFIX, host.as_bytes()].concat()
    }
}

crate::impl_deser! {
    profile, GetUserResponse, 5096;
    invite, Invite, 1024;
    message, HarmonyMessage, 10192;
    guild, GetGuildResponse, 1024;
    chan, Channel, 1024;
    role, Role, 1024;
    perm_list, PermissionList, 1024;
}

pub fn deser_invite_entry_guild_id(data: &sled::IVec) -> u64 {
    use std::convert::TryInto;

    let (id_raw, _) = data.split_at(size_of::<u64>());
    u64::from_be_bytes(unsafe { id_raw.try_into().unwrap_unchecked() })
}

#[cached(size = 1024)]
pub fn deser_invite_entry(data: sled::IVec) -> (u64, Invite) {
    let guild_id = deser_invite_entry_guild_id(&data);
    let (_, invite_raw) = data.split_at(size_of::<u64>());
    let invite = deser_invite(invite_raw.into());

    (guild_id, invite)
}

#[macro_export]
macro_rules! impl_deser {
    ( $( $name:ident, $msg:ty, $size:expr; )* ) => {
        paste::paste! {
            $(
                #[cached(size = $size)]
                pub fn [<deser_ $name>](data: sled::IVec) -> $msg {
                    $msg::decode(data.as_ref()).unwrap()
                }
            )*
        }
    };
}

const fn concat_static<const LEN: usize>(arrs: &[&[u8]]) -> [u8; LEN] {
    let mut new = [0_u8; LEN];

    let mut new_index = 0;
    let mut arr_index = 0;
    while arr_index < arrs.len() {
        let arr = arrs[arr_index];
        let mut arr_from_index = 0;
        while arr_from_index < arr.len() {
            new[new_index] = arr[arr_from_index];
            new_index += 1;
            arr_from_index += 1;
        }
        arr_index += 1;
    }
    new
}
