//! This module contains the DB abstraction, DB keys and migrations.
//! The DB abstraction is taken straight from sled and so fits most to it,
//! but is essentially an abstraction for any KV map.
//!
//! scherzo uses big endian for keys, as it was first written for sled.
//!
//! scherzo uses:
//! - `chat` tree for chat service,
//! - `auth` tree for auth service,
//! - `emote` tree for emote service,
//! - `profile` tree for profile service,
//! - and `sync` tree for sync service.
//!
//! `chat` service general structure:
//! - guild keys are simply guild ids but serialized (`.to_be_bytes()`)
//! - channel keys are `guild id + seperator + channel id`
//! - messages are `channel key + seperator + message id`
//! - these make it very easy to delete stuff related to guilds / channels / messages
//! as it is just a simple `.scan_prefix()` call followed by a batch remove
//!
//! `auth` service general struture:
//! - `token prefix + user id` -> token
//! - `atime prefix + user id` -> last valid time (what?)
//! - email -> user id
//! - user id -> hashed password
//!
//! TODO other stuff

use std::{
    convert::TryInto,
    error::Error as StdError,
    fmt::{self, Display, Formatter},
    mem::size_of,
};

use crate::{config::DbConfig, error::travel_error, utils::evec::EVec, ServerError, ServerResult};

use harmony_rust_sdk::api::{
    chat::{Channel, Guild, Invite, Message as HarmonyMessage, Role},
    emote::{Emote, EmotePack},
    profile::Profile,
};
use rkyv::{
    archived_root,
    de::deserializers::SharedDeserializeMap,
    ser::{serializers::AllocSerializer, Serializer},
    AlignedVec, Archive, Deserialize, Serialize,
};
use tracing::Instrument;

pub mod migration;
#[cfg(feature = "sled")]
pub mod sled;
#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(feature = "sled")]
pub use self::sled::shared::*;
#[cfg(all(feature = "sqlite", not(feature = "sled")))]
pub use self::sqlite::shared::*;

pub const TREES: [&[u8]; 6] = [b"auth", b"chat", b"sync", b"version", b"profile", b"emote"];

pub async fn open_db(db_path: String, db_config: DbConfig) -> Db {
    let span = tracing::info_span!("scherzo::db", path = %db_path);
    let fut = async move {
        tracing::info!("initializing database");

        let db_result = open_database(db_path, db_config).await;

        match db_result {
            Ok(db) => db,
            Err(err) => {
                tracing::error!("cannot open database: {}; aborting", err);

                std::process::exit(1);
            }
        }
    };
    fut.instrument(span).await
}

#[must_use]
#[derive(Default)]
pub struct Batch {
    inserts: Vec<(EVec, Option<EVec>)>,
}

impl Batch {
    pub fn insert(&mut self, key: impl Into<EVec>, value: impl Into<EVec>) {
        self.inserts.push((key.into(), Some(value.into())));
    }

    pub fn remove(&mut self, key: impl Into<EVec>) {
        self.inserts.push((key.into(), None));
    }
}

#[derive(Debug)]
pub struct DbError {
    pub inner: Box<dyn StdError + Sync + Send>,
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

pub fn make_u64_iter_logic(raw: &[u8]) -> impl Iterator<Item = u64> + '_ {
    raw.chunks_exact(size_of::<u64>()).map(deser_id)
}

pub mod profile {
    use super::concat_static;

    pub const USER_PREFIX: &[u8] = b"user_";
    pub const FOREIGN_PREFIX: &[u8] = b"fuser_";

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

    pub fn make_user_metadata_prefix(user_id: u64) -> [u8; 14] {
        concat_static(&[make_user_profile_key(user_id).as_ref(), [1].as_ref()])
    }

    pub fn make_user_metadata_key(user_id: u64, app_id: &str) -> Vec<u8> {
        [
            make_user_metadata_prefix(user_id).as_ref(),
            app_id.as_bytes(),
        ]
        .concat()
    }
}

pub mod emote {
    use super::{concat_static, profile::USER_PREFIX};

    pub const EMOTEPACK_PREFIX: &[u8] = b"emotep_";

    pub const fn make_emote_pack_key(pack_id: u64) -> [u8; 15] {
        concat_static(&[EMOTEPACK_PREFIX, &pack_id.to_be_bytes()])
    }

    pub fn make_emote_pack_emote_key(pack_id: u64, image_id: &str) -> Vec<u8> {
        [
            EMOTEPACK_PREFIX,
            &pack_id.to_be_bytes(),
            image_id.as_bytes(),
        ]
        .concat()
    }

    pub const fn make_equipped_emote_prefix(user_id: u64) -> [u8; 14] {
        concat_static(&[USER_PREFIX, &user_id.to_be_bytes(), &[9]])
    }

    pub const fn make_equipped_emote_key(user_id: u64, pack_id: u64) -> [u8; 22] {
        concat_static(&[&make_equipped_emote_prefix(user_id), &pack_id.to_be_bytes()])
    }
}

pub mod chat {
    use super::concat_static;

    pub const INVITE_PREFIX: &[u8] = b"invite_";
    pub const ADMIN_GUILD_KEY: &[u8] = b"admin_guild_key_data";

    // perms

    pub fn make_guild_perm_key(guild_id: u64, role_id: u64, matches: &str) -> Vec<u8> {
        [
            &make_role_guild_perms_prefix(guild_id, role_id),
            matches.as_bytes(),
        ]
        .concat()
    }

    pub fn make_channel_perm_key(
        guild_id: u64,
        channel_id: u64,
        role_id: u64,
        matches: &str,
    ) -> Vec<u8> {
        [
            &make_role_channel_perms_prefix(guild_id, channel_id, role_id),
            matches.as_bytes(),
        ]
        .concat()
    }

    pub const fn make_role_guild_perms_prefix(guild_id: u64, role_id: u64) -> [u8; 18] {
        concat_static(&[
            &make_guild_role_prefix(guild_id),
            &role_id.to_be_bytes(),
            &[9],
        ])
    }

    pub const fn make_guild_role_key(guild_id: u64, role_id: u64) -> [u8; 17] {
        concat_static(&[&make_guild_role_prefix(guild_id), &role_id.to_be_bytes()])
    }

    pub const fn make_guild_user_roles_key(guild_id: u64, user_id: u64) -> [u8; 17] {
        concat_static(&[
            &make_guild_user_roles_prefix(guild_id),
            &user_id.to_be_bytes(),
        ])
    }

    // perms

    // message

    pub const fn make_pinned_msgs_key(guild_id: u64, channel_id: u64) -> [u8; 18] {
        concat_static(&[&make_chan_key(guild_id, channel_id), &[6]])
    }

    pub const fn make_next_msg_id_key(guild_id: u64, channel_id: u64) -> [u8; 18] {
        concat_static(&[&make_chan_key(guild_id, channel_id), &[7]])
    }

    pub const fn make_role_channel_perms_prefix(
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

    pub const fn make_msg_prefix(guild_id: u64, channel_id: u64) -> [u8; 18] {
        concat_static(&[&make_chan_key(guild_id, channel_id), &[9]])
    }

    pub const fn make_msg_key(guild_id: u64, channel_id: u64, message_id: u64) -> [u8; 26] {
        concat_static(&[
            &make_msg_prefix(guild_id, channel_id),
            &message_id.to_be_bytes(),
        ])
    }

    pub fn make_user_reacted_msg_key(
        guild_id: u64,
        channel_id: u64,
        message_id: u64,
        user_id: u64,
        image_id: &str,
    ) -> Vec<u8> {
        [
            make_msg_key(guild_id, channel_id, message_id).as_ref(),
            &[0],
            user_id.to_be_bytes().as_ref(),
            image_id.as_bytes(),
        ]
        .concat()
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
    pub const AUTH_PREFIX: &[u8] = b"auth_";
    pub const SU_TOKEN_PREFIX: &[u8] = b"reg_token_";

    pub fn auth_key(token: &str) -> Vec<u8> {
        [AUTH_PREFIX, token.as_bytes()].concat()
    }

    pub const fn token_key(user_id: u64) -> [u8; 14] {
        concat_static(&[TOKEN_PREFIX, &user_id.to_be_bytes()])
    }

    pub const fn atime_key(user_id: u64) -> [u8; 14] {
        concat_static(&[ATIME_PREFIX, &user_id.to_be_bytes()])
    }

    pub fn single_use_token_key(token_hashed: &[u8]) -> Vec<u8> {
        [SU_TOKEN_PREFIX, token_hashed].concat()
    }
}

pub mod sync {
    pub const HOST_PREFIX: &[u8] = b"host_";

    pub fn make_host_key(host: &str) -> Vec<u8> {
        [HOST_PREFIX, host.as_bytes()].concat()
    }
}

pub async fn batch_delete_prefix(tree: &Tree, prefix: impl AsRef<[u8]>) -> ServerResult<()> {
    let batch =
        tree.scan_prefix(prefix.as_ref())
            .await
            .try_fold(Batch::default(), |mut batch, res| {
                let (key, _) = res.map_err(ServerError::from)?;
                batch.remove(key);
                ServerResult::Ok(batch)
            })?;
    tree.apply_batch(batch).await?;
    Ok(())
}

pub fn deser_id(data: impl AsRef<[u8]>) -> u64 {
    u64::from_be_bytes(data.as_ref().try_into().expect("length wasnt 8"))
}

crate::impl_deser! {
    profile, Profile;
    invite, Invite;
    message, HarmonyMessage;
    guild, Guild;
    chan, Channel;
    role, Role;
    emote, Emote;
    emote_pack, EmotePack;
}

pub fn deser_invite_entry_guild_id(data: &[u8]) -> u64 {
    let (id_raw, _) = data.split_at(size_of::<u64>());
    deser_id(id_raw)
}

pub fn deser_invite_entry(data: EVec) -> (u64, Invite) {
    let guild_id = deser_invite_entry_guild_id(&data);
    let (_, invite_raw) = data.split_at(size_of::<u64>());
    let mut data = AlignedVec::new();
    data.extend_from_slice(invite_raw);
    let invite = deser_invite(data);

    (guild_id, invite)
}

#[macro_export]
macro_rules! impl_deser {
    ( $( $name:ident, $msg:ty; )* ) => {
        paste::paste! {
            $(
                pub fn [<deser_ $name>](data: impl AsRef<[u8]>) -> $msg {
                    let archive = rkyv_arch::<$msg>(data.as_ref());
                    archive.deserialize(&mut SharedDeserializeMap::default()).expect("failed to deserialize")
                }
            )*
        }
    };
}

const fn concat_static<const LEN: usize>(arrs: &[&[u8]]) -> [u8; LEN] {
    // check if the lengths match.
    {
        let mut new_len = 0;
        let mut arr_index = 0;
        while arr_index < arrs.len() {
            let arr = arrs[arr_index];
            new_len += arr.len();
            arr_index += 1;
        }
        if new_len != LEN {
            panic!("concatted array len does not match the specified length");
        }
    }

    let mut new = [0_u8; LEN];

    let mut arr_index = 0;
    let mut padding = 0;
    while arr_index < arrs.len() {
        let arr = arrs[arr_index];
        // SAFETY:
        // - the pointers dont overlap because we use our own allocated array
        // - the pointers are guaranteed to be not null (we create one in fn body
        // others are gotten as references
        // - the pointers are guaranteed to be aligned (are they?)
        unsafe {
            std::ptr::copy_nonoverlapping(arr.as_ptr(), new.as_mut_ptr().add(padding), arr.len());
        }
        padding += arr.len();
        arr_index += 1;
    }

    new
}

pub fn rkyv_ser<Value: Serialize<AllocSerializer<1024>>>(value: &Value) -> AlignedVec {
    let mut ser = AllocSerializer::<1024>::default();
    ser.serialize_value(value).unwrap();
    ser.into_serializer().into_inner()
}

pub fn rkyv_arch<As: Archive>(data: &[u8]) -> &<As as Archive>::Archived {
    unsafe { archived_root::<As>(data) }
}
