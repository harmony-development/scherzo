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
    use crate::concat_static;

    pub const USER_PREFIX: [u8; 5] = [b'u', b's', b'e', b'r', b'_'];
    pub const INVITE_PREFIX: [u8; 7] = [b'i', b'n', b'v', b'i', b't', b'e', b'_'];

    // message

    pub fn make_msg_prefix(guild_id: u64, channel_id: u64) -> [u8; 18] {
        concat_static!(18, make_chan_key(guild_id, channel_id), [9])
    }

    pub fn make_msg_key(guild_id: u64, channel_id: u64, message_id: u64) -> [u8; 26] {
        concat_static!(
            26,
            make_msg_prefix(guild_id, channel_id),
            message_id.to_be_bytes()
        )
    }

    // message

    // member

    pub fn make_member_key(guild_id: u64, user_id: u64) -> [u8; 17] {
        concat_static!(17, make_guild_mem_prefix(guild_id), user_id.to_be_bytes())
    }

    pub fn make_banned_member_key(guild_id: u64, user_id: u64) -> [u8; 17] {
        concat_static!(
            17,
            make_guild_banned_mem_prefix(guild_id),
            user_id.to_be_bytes()
        )
    }

    pub fn make_member_profile_key(user_id: u64) -> [u8; 13] {
        concat_static!(13, USER_PREFIX, user_id.to_be_bytes())
    }

    // member

    // guild

    pub fn make_guild_mem_prefix(guild_id: u64) -> [u8; 9] {
        concat_static!(9, guild_id.to_be_bytes(), [9])
    }

    pub fn make_guild_chan_prefix(guild_id: u64) -> [u8; 9] {
        concat_static!(9, guild_id.to_be_bytes(), [8])
    }

    pub fn make_guild_banned_mem_prefix(guild_id: u64) -> [u8; 9] {
        concat_static!(9, guild_id.to_be_bytes(), [7])
    }

    pub fn make_guild_role_prefix(guild_id: u64) -> [u8; 9] {
        concat_static!(9, guild_id.to_be_bytes(), [5])
    }

    pub fn make_guild_user_roles_prefix(guild_id: u64) -> [u8; 9] {
        concat_static!(9, guild_id.to_be_bytes(), [4])
    }

    pub fn make_guild_default_role_key(guild_id: u64) -> [u8; 10] {
        concat_static!(10, guild_id.to_be_bytes(), [1, 4])
    }

    pub fn make_guild_role_ordering_key(guild_id: u64) -> [u8; 10] {
        concat_static!(10, guild_id.to_be_bytes(), [1, 3])
    }

    pub fn make_guild_list_key_prefix(user_id: u64) -> [u8; 10] {
        concat_static!(10, user_id.to_be_bytes(), [1, 2])
    }

    pub fn make_guild_chan_ordering_key(guild_id: u64) -> [u8; 10] {
        concat_static!(10, guild_id.to_be_bytes(), [1, 1])
    }

    pub fn make_guild_list_key(user_id: u64, guild_id: u64, host: &str) -> Vec<u8> {
        [
            make_guild_list_key_prefix(user_id).as_ref(),
            guild_id.to_be_bytes().as_ref(),
            host.as_bytes(),
        ]
        .concat()
    }

    pub fn make_guild_role_key(guild_id: u64, role_id: u64) -> [u8; 17] {
        concat_static!(17, make_guild_role_prefix(guild_id), role_id.to_be_bytes())
    }

    pub fn make_guild_role_perms_key(guild_id: u64, role_id: u64) -> [u8; 18] {
        concat_static!(
            18,
            make_guild_role_prefix(guild_id),
            role_id.to_be_bytes(),
            [9]
        )
    }

    pub fn make_guild_user_roles_key(guild_id: u64, user_id: u64) -> [u8; 17] {
        concat_static!(
            17,
            make_guild_user_roles_prefix(guild_id),
            user_id.to_be_bytes()
        )
    }

    pub fn make_guild_channel_roles_key(guild_id: u64, channel_id: u64, role_id: u64) -> [u8; 26] {
        concat_static!(
            26,
            make_chan_key(guild_id, channel_id),
            [8],
            role_id.to_be_bytes()
        )
    }

    // guild

    pub fn make_chan_key(guild_id: u64, channel_id: u64) -> [u8; 17] {
        concat_static!(
            17,
            make_guild_chan_prefix(guild_id),
            channel_id.to_be_bytes()
        )
    }

    pub fn make_invite_key(name: &str) -> Vec<u8> {
        [&INVITE_PREFIX, name.as_bytes()].concat()
    }
}

pub mod auth {
    use crate::concat_static;

    pub const ATIME_PREFIX: [u8; 6] = [b'a', b't', b'i', b'm', b'e', b'_'];
    pub const TOKEN_PREFIX: [u8; 6] = [b't', b'o', b'k', b'e', b'n', b'_'];

    pub fn token_key(user_id: u64) -> [u8; 14] {
        concat_static!(14, TOKEN_PREFIX, user_id.to_be_bytes())
    }

    pub fn atime_key(user_id: u64) -> [u8; 14] {
        concat_static!(14, ATIME_PREFIX, user_id.to_be_bytes())
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
