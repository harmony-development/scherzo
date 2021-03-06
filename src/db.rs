use std::{
    convert::TryInto,
    error::Error as StdError,
    fmt::{self, Display, Formatter},
    mem::size_of,
    ops::RangeInclusive,
    sync::Arc,
};

use crate::travel_error;
use cached::proc_macro::cached;
use harmony_rust_sdk::api::{
    chat::{
        get_guild_channels_response::Channel, get_guild_invites_response::Invite, GetGuildResponse,
        GetUserResponse, PermissionList, Role,
    },
    exports::prost::Message,
    harmonytypes::Message as HarmonyMessage,
};

pub mod noop;
#[cfg(feature = "sled")]
pub mod sled;

#[derive(Default)]
pub struct Batch {
    inserts: Vec<(Vec<u8>, Option<Vec<u8>>)>,
}

impl Batch {
    pub fn insert(&mut self, key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) {
        self.inserts
            .push((key.as_ref().to_vec(), Some(value.as_ref().to_vec())));
    }

    pub fn remove(&mut self, key: impl AsRef<[u8]>) {
        self.inserts.push((key.as_ref().to_vec(), None));
    }
}

#[derive(Debug)]
pub struct DbError {
    pub inner: Box<dyn StdError>,
}

impl Display for DbError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        travel_error(f, self.inner.as_ref());
        f.write_str("a database error occured")
    }
}

impl StdError for DbError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.inner.as_ref())
    }
}

pub type DbResult<T> = Result<T, DbError>;
pub type ArcTree = Arc<dyn Tree>;

pub trait Db {
    fn open_tree(&self, name: &[u8]) -> DbResult<ArcTree>;
}

type Iter<'a> = Box<dyn Iterator<Item = DbResult<(Vec<u8>, Vec<u8>)>> + Send + 'a>;
type RangeIter<'a> = Box<dyn DoubleEndedIterator<Item = DbResult<(Vec<u8>, Vec<u8>)>> + Send + 'a>;

pub trait Tree: Send + Sync {
    fn get(&self, key: &[u8]) -> DbResult<Option<Vec<u8>>>;
    fn insert(&self, key: &[u8], value: &[u8]) -> DbResult<Option<Vec<u8>>>;
    fn remove(&self, key: &[u8]) -> DbResult<Option<Vec<u8>>>;
    fn scan_prefix<'a>(&'a self, prefix: &[u8]) -> Iter<'a>;
    fn apply_batch(&self, batch: Batch) -> DbResult<()>;
    fn contains_key(&self, key: &[u8]) -> DbResult<bool>;
    fn range<'a>(&'a self, range: RangeInclusive<&[u8]>) -> RangeIter<'a>;
    fn verify_integrity(&self) -> DbResult<()>;
}

pub fn make_u64_iter_logic(raw: &[u8]) -> impl Iterator<Item = u64> + '_ {
    raw.chunks_exact(size_of::<u64>())
        .map(|raw| u64::from_be_bytes(unsafe { raw.try_into().unwrap_unchecked() }))
}

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

    pub const fn make_local_to_foreign_user_key(local_id: u64) -> [u8; 15] {
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

pub fn deser_invite_entry_guild_id(data: &[u8]) -> u64 {
    let (id_raw, _) = data.split_at(size_of::<u64>());
    u64::from_be_bytes(unsafe { id_raw.try_into().unwrap_unchecked() })
}

#[cached(size = 1024)]
pub fn deser_invite_entry(data: Vec<u8>) -> (u64, Invite) {
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
                pub fn [<deser_ $name>](data: Vec<u8>) -> $msg {
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
