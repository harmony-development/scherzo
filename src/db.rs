use crate::concat_static;

pub fn make_msg_prefix(guild_id: u64, channel_id: u64) -> [u8; 18] {
    concat_static!(18, make_chan_key(guild_id, channel_id), [9])
}

pub fn make_msg_key(guild_id: u64, channel_id: u64, message_id: u64) -> [u8; 26] {
    concat_static!(
        26,
        make_msg_prefix(guild_id, channel_id),
        message_id.to_le_bytes()
    )
}

pub fn make_chan_key(guild_id: u64, channel_id: u64) -> [u8; 17] {
    concat_static!(
        17,
        make_guild_chan_prefix(guild_id),
        channel_id.to_le_bytes()
    )
}

pub fn make_guild_chan_prefix(guild_id: u64) -> [u8; 9] {
    concat_static!(9, guild_id.to_le_bytes(), [8])
}

pub fn make_member_key(guild_id: u64, user_id: u64) -> [u8; 17] {
    concat_static!(17, make_guild_mem_prefix(guild_id), user_id.to_le_bytes())
}

pub fn make_guild_mem_prefix(guild_id: u64) -> [u8; 9] {
    concat_static!(9, guild_id.to_le_bytes(), [9])
}

pub fn make_guild_list_key_prefix(user_id: u64) -> [u8; 10] {
    concat_static!(10, user_id.to_le_bytes(), [1, 2])
}

pub fn make_guild_list_key(user_id: u64, guild_id: u64, host: &str) -> Vec<u8> {
    [
        make_guild_list_key_prefix(user_id).as_ref(),
        guild_id.to_le_bytes().as_ref(),
        host.as_bytes(),
    ]
    .concat()
}

pub fn make_invite_key(name: &str) -> Vec<u8> {
    [&INVITE_PREFIX, name.as_bytes()].concat()
}

pub fn make_member_profile_key(user_id: u64) -> [u8; 13] {
    concat_static!(13, USER_PREFIX, user_id.to_le_bytes())
}

pub const USER_PREFIX: [u8; 5] = [b'u', b's', b'e', b'r', b'_'];
pub const INVITE_PREFIX: [u8; 7] = [b'i', b'n', b'v', b'i', b't', b'e', b'_'];
