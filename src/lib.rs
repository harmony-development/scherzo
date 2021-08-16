#![feature(option_result_unwrap_unchecked)]
#![allow(clippy::unit_arg, clippy::blocks_in_if_conditions)]

use std::{
    fmt::{self, Display, Formatter, Write},
    io::Error as IoError,
    sync::atomic::AtomicBool,
    time::Duration,
};

use harmony_rust_sdk::api::exports::hrpc::{
    encode_protobuf_message, http,
    server::{CustomError, StatusCode},
    url::ParseError as UrlParseError,
    warp::{self, reply::Response},
};
use parking_lot::Mutex;
use reqwest::Url;
use smol_str::SmolStr;
use triomphe::Arc;

pub mod append_list;
pub mod config;
pub mod db;
pub mod impls;
pub mod key;

pub static DISABLE_RATELIMITS: AtomicBool = AtomicBool::new(false);
pub const HARMONY_PROTO_NAME: &str = "harmony";
pub const SCHERZO_VERSION: &str = git_version::git_version!(
    prefix = "git:",
    cargo_prefix = "cargo:",
    fallback = "unknown"
);

pub fn set_proto_name(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(http::header::SEC_WEBSOCKET_PROTOCOL, unsafe {
            http::HeaderValue::from_maybe_shared_unchecked(HARMONY_PROTO_NAME)
        });
    response
}

pub type ServerResult<T> = Result<T, ServerError>;

pub type SharedConfig = Arc<Mutex<SharedConfigData>>;
#[derive(Default)]
pub struct SharedConfigData {
    pub motd: String,
}

#[derive(Debug)]
pub enum ServerError {
    InvalidUrl(UrlParseError),
    InvalidAuthId,
    NoFieldSpecified,
    NoSuchField,
    NoSuchChoice {
        choice: SmolStr,
        expected_any_of: Vec<SmolStr>,
    },
    WrongStep {
        expected: SmolStr,
        got: SmolStr,
    },
    WrongTypeForField {
        name: SmolStr,
        expected: SmolStr,
    },
    WrongUserOrPassword {
        email: SmolStr,
    },
    UserBanned,
    UserAlreadyInGuild,
    UserAlreadyExists,
    UserNotInGuild {
        guild_id: u64,
        user_id: u64,
    },
    Unauthenticated,
    NotImplemented,
    NoSuchMessage {
        guild_id: u64,
        channel_id: u64,
        message_id: u64,
    },
    GuildAlreadyExists(u64),
    NoSuchGuild(u64),
    ChannelAlreadyExists {
        guild_id: u64,
        channel_id: u64,
    },
    NoSuchChannel {
        guild_id: u64,
        channel_id: u64,
    },
    NoSuchInvite(SmolStr),
    NoSuchUser(u64),
    InternalServerError,
    SessionExpired,
    NotEnoughPermissions {
        must_be_guild_owner: bool,
        missing_permission: SmolStr,
    },
    EmptyPermissionQuery,
    NoSuchRole {
        guild_id: u64,
        role_id: u64,
    },
    NoRoleSpecified,
    NoPermissionsSpecified,
    MissingFiles,
    TooManyFiles,
    WarpError(warp::Error),
    IoError(IoError),
    InvalidFileId,
    ReqwestError(reqwest::Error),
    Unexpected(SmolStr),
    NotAnImage,
    TooFast(Duration),
    MediaNotFound,
    InviteExpired,
    FailedToAuthSync,
    CantGetKey,
    CantGetHostKey(SmolStr),
    InvalidTokenData,
    InvalidTokenSignature,
    InvalidTime,
    CouldntVerifyTokenData,
    InvalidToken,
    FederationDisabled,
    HostNotAllowed,
    EmotePackNotFound,
    NotEmotePackOwner,
    LinkNotFound(Url),
    MessageContentCantBeEmpty,
}

impl Display for ServerError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            ServerError::InviteExpired => write!(f, "invite expired"),
            ServerError::InvalidUrl(err) => write!(f, "invalid URL: {}", err),
            ServerError::TooFast(rem) => write!(f, "too fast, try again in {}", rem.as_secs_f64()),
            ServerError::InvalidAuthId => write!(f, "invalid auth id"),
            ServerError::NoFieldSpecified => write!(f, "expected field in response"),
            ServerError::NoSuchField => write!(f, "no such field"),
            ServerError::NoSuchChoice {
                choice,
                expected_any_of,
            } => {
                write!(
                    f,
                    "no such choice {}, expected any of {:?}",
                    choice, expected_any_of
                )
            }
            ServerError::WrongStep { expected, got } => {
                write!(
                    f,
                    "wrong step type in response, expected {}, got {}",
                    expected, got
                )
            }
            ServerError::WrongTypeForField { name, expected } => {
                write!(f, "wrong type for field {}, expected {}", name, expected)
            }
            ServerError::WrongUserOrPassword { email } => {
                write!(f, "wrong email or password for email {}", email)
            }
            ServerError::UserBanned => write!(f, "user banned in guild"),
            ServerError::UserNotInGuild { guild_id, user_id } => {
                write!(f, "user {} not in guild {}", user_id, guild_id)
            }
            ServerError::UserAlreadyExists => write!(f, "user already exists"),
            ServerError::UserAlreadyInGuild => write!(f, "user already in guild"),
            ServerError::Unauthenticated => write!(f, "invalid-session"),
            ServerError::NotImplemented => write!(f, "not implemented"),
            ServerError::NoSuchChannel {
                guild_id,
                channel_id,
            } => write!(
                f,
                "channel {} does not exist in guild {}",
                channel_id, guild_id
            ),
            ServerError::ChannelAlreadyExists {
                guild_id,
                channel_id,
            } => write!(
                f,
                "channel {} already exists in guild {}",
                channel_id, guild_id
            ),
            ServerError::GuildAlreadyExists(guild_id) => {
                write!(f, "guild {} already exists", guild_id)
            }
            ServerError::NoSuchGuild(id) => write!(f, "no such guild with id {}", id),
            ServerError::NoSuchUser(id) => write!(f, "no such user with id {}", id),
            ServerError::NoSuchMessage {
                guild_id,
                channel_id,
                message_id,
            } => write!(
                f,
                "no such message {} in channel {} in guild {}",
                message_id, channel_id, guild_id
            ),
            ServerError::NoSuchInvite(id) => write!(f, "no such invite with id {}", id),
            ServerError::InternalServerError => write!(f, "internal server error"),
            ServerError::SessionExpired => write!(f, "session expired"),
            ServerError::NotEnoughPermissions {
                must_be_guild_owner,
                missing_permission,
            } => {
                f.write_str("missing permissions: \n")?;
                if *must_be_guild_owner {
                    f.write_str("must be guild owner\n")?;
                }
                f.write_str(missing_permission)?;
                f.write_char('\n')
            }
            ServerError::EmptyPermissionQuery => write!(f, "permission query cant be empty"),
            ServerError::NoSuchRole { guild_id, role_id } => {
                write!(f, "no such role {} in guild {}", role_id, guild_id)
            }
            ServerError::NoRoleSpecified => write!(
                f,
                "no role specified when there must have been one specified"
            ),
            ServerError::NoPermissionsSpecified => write!(
                f,
                "no permissions specified when there must have been some specified"
            ),
            ServerError::TooManyFiles => write!(f, "uploaded too many files"),
            ServerError::MissingFiles => write!(f, "must upload at least one file"),
            ServerError::WarpError(err) => {
                travel_error(f, err);
                write!(f, "error occured in warp: {}", err)
            }
            ServerError::IoError(err) => {
                travel_error(f, err);
                write!(f, "io error occured: {}", err)
            }
            ServerError::ReqwestError(err) => {
                travel_error(f, err);
                write!(f, "error occured in reqwest: {}", err)
            }
            ServerError::Unexpected(msg) => write!(f, "unexpected behaviour: {}", msg),
            ServerError::InvalidFileId => write!(f, "invalid file id"),
            ServerError::NotAnImage => write!(f, "the requested URL does not point to an image"),
            ServerError::MediaNotFound => write!(f, "requested media is not found"),
            ServerError::FailedToAuthSync => write!(f, "failed to auth for host"),
            ServerError::CantGetKey => write!(f, "can't get key"),
            ServerError::CantGetHostKey(host) => write!(f, "can't get host key: {}", host),
            ServerError::InvalidTokenData => write!(f, "token data is invalid"),
            ServerError::InvalidTokenSignature => write!(f, "token signature is invalid"),
            ServerError::InvalidTime => write!(f, "invalid time"),
            ServerError::InvalidToken => write!(f, "token is invalid"),
            ServerError::CouldntVerifyTokenData => write!(
                f,
                "token data could not be verified with the given signature"
            ),
            ServerError::FederationDisabled => write!(f, "federation is disabled on this server"),
            ServerError::HostNotAllowed => write!(f, "host is not allowed on this server"),
            ServerError::EmotePackNotFound => write!(f, "emote pack is not found"),
            ServerError::NotEmotePackOwner => write!(f, "you are not the owner of this emote pack"),
            ServerError::LinkNotFound(url) => {
                write!(f, "metadata requested for link {} not found", url)
            }
            ServerError::MessageContentCantBeEmpty => write!(f, "message content cannot be empty"),
        }
    }
}

impl CustomError for ServerError {
    fn code(&self) -> StatusCode {
        match self {
            ServerError::InvalidAuthId
            | ServerError::NoFieldSpecified
            | ServerError::NoSuchField
            | ServerError::NoSuchChoice { .. }
            | ServerError::WrongStep { .. }
            | ServerError::WrongTypeForField { .. }
            | ServerError::WrongUserOrPassword { .. }
            | ServerError::UserAlreadyExists
            | ServerError::UserNotInGuild { .. }
            | ServerError::Unauthenticated
            | ServerError::NoSuchGuild(_)
            | ServerError::NoSuchInvite(_)
            | ServerError::NoSuchMessage { .. }
            | ServerError::NoSuchUser(_)
            | ServerError::NotEnoughPermissions { .. }
            | ServerError::SessionExpired
            | ServerError::UserBanned
            | ServerError::UserAlreadyInGuild
            | ServerError::NoSuchChannel { .. }
            | ServerError::ChannelAlreadyExists { .. }
            | ServerError::GuildAlreadyExists(_)
            | ServerError::EmptyPermissionQuery
            | ServerError::NoRoleSpecified
            | ServerError::NoSuchRole { .. }
            | ServerError::NoPermissionsSpecified
            | ServerError::TooManyFiles
            | ServerError::MissingFiles
            | ServerError::InvalidFileId
            | ServerError::NotAnImage
            | ServerError::InvalidUrl(_)
            | ServerError::InviteExpired
            | ServerError::FailedToAuthSync
            | ServerError::InvalidTokenData
            | ServerError::InvalidTokenSignature
            | ServerError::InvalidTime
            | ServerError::CouldntVerifyTokenData
            | ServerError::InvalidToken
            | ServerError::EmotePackNotFound
            | ServerError::NotEmotePackOwner
            | ServerError::MessageContentCantBeEmpty => StatusCode::BAD_REQUEST,
            ServerError::FederationDisabled | ServerError::HostNotAllowed => StatusCode::FORBIDDEN,
            ServerError::WarpError(_)
            | ServerError::IoError(_)
            | ServerError::InternalServerError
            | ServerError::ReqwestError(_)
            | ServerError::Unexpected(_)
            | ServerError::CantGetKey
            | ServerError::CantGetHostKey(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ServerError::TooFast(_) => StatusCode::TOO_MANY_REQUESTS,
            ServerError::MediaNotFound | ServerError::LinkNotFound(_) => StatusCode::NOT_FOUND,
            ServerError::NotImplemented => StatusCode::NOT_IMPLEMENTED,
        }
    }

    fn message(&self) -> Vec<u8> {
        let i18n_code = match self {
            ServerError::InternalServerError
            | ServerError::WarpError(_)
            | ServerError::IoError(_)
            | ServerError::ReqwestError(_)
            | ServerError::Unexpected(_)
            | ServerError::CantGetKey
            | ServerError::CantGetHostKey(_) => "h.internal-server-error",
            ServerError::Unauthenticated => "h.blank-session",
            ServerError::InvalidAuthId => "h.bad-auth-id",
            ServerError::UserAlreadyExists => "h.already-registered",
            ServerError::UserAlreadyInGuild => "h.already-in-guild",
            ServerError::UserBanned => "h.banned-from-guild",
            ServerError::NotEnoughPermissions {
                must_be_guild_owner,
                ..
            } => {
                if *must_be_guild_owner {
                    "h.not-owner"
                } else {
                    "h.not-enough-permissions"
                }
            }
            ServerError::NoFieldSpecified => "h.missing-form",
            ServerError::NoSuchField => "h.missing-form",
            ServerError::NoSuchChoice { .. } => "h.bad-auth-choice",
            ServerError::WrongStep { .. } => "h.bad-auth-choice",
            ServerError::WrongTypeForField { .. } => "h.missing-form",
            ServerError::WrongUserOrPassword { .. } => "h.bad-password\nh.bad-email",
            ServerError::UserNotInGuild { .. } => "h.not-joined",
            ServerError::NotImplemented => "h.not-implemented",
            ServerError::ChannelAlreadyExists { .. } => "h.channel-already-exists",
            ServerError::GuildAlreadyExists(_) => "h.guild-already-exists",
            ServerError::NoSuchMessage { .. } => "h.bad-message-id",
            ServerError::NoSuchChannel { .. } => "h.bad-channel-id",
            ServerError::NoSuchGuild(_) => "h.bad-guild-id",
            ServerError::NoSuchInvite(_) => "h.bad-invite-id",
            ServerError::NoSuchUser(_) => "h.bad-user-id",
            ServerError::SessionExpired => "h.bad-session",
            ServerError::EmptyPermissionQuery => "h.permission-query-empty",
            ServerError::NoSuchRole { .. } => "h.bad-role-id",
            ServerError::NoRoleSpecified => "h.missing-role",
            ServerError::NoPermissionsSpecified => "h.missing-permissions",
            ServerError::InvalidFileId => "h.bad-file-id",
            ServerError::TooManyFiles => return "too-many-files".as_bytes().to_vec(),
            ServerError::MissingFiles => return "missing-files".as_bytes().to_vec(),
            ServerError::TooFast(_) => "h.rate-limited",
            ServerError::NotAnImage => return "not-an-image".as_bytes().to_vec(),
            ServerError::MediaNotFound | ServerError::LinkNotFound(_) => {
                return Self::NOT_FOUND_ERROR.1.to_vec()
            }
            ServerError::InvalidUrl(_) => "h.bad-url",
            ServerError::InviteExpired => "h.bad-invite-id",
            ServerError::FailedToAuthSync => "h.bad-auth",
            ServerError::InvalidTokenData => "h.bad-token-data",
            ServerError::InvalidTokenSignature => "h.bad-token-signature",
            ServerError::InvalidTime => "h.bad-time",
            ServerError::CouldntVerifyTokenData => "h.token-verify-failure",
            ServerError::InvalidToken => "h.bad-token",
            ServerError::FederationDisabled => "h.federation-disabled",
            ServerError::HostNotAllowed => "h.host-not-allowed",
            ServerError::NotEmotePackOwner => "h.not-emote-pack-owner",
            ServerError::EmotePackNotFound => "h.emote-pack-not-found",
            ServerError::MessageContentCantBeEmpty => "h.message-content-empty",
        };
        encode_protobuf_message(harmony_rust_sdk::api::harmonytypes::Error {
            identifier: i18n_code.into(),
            human_message: self.to_string(),
            more_details: Vec::new(),
        })
        .to_vec()
    }
}

impl From<IoError> for ServerError {
    fn from(err: IoError) -> Self {
        ServerError::IoError(err)
    }
}

impl From<warp::Error> for ServerError {
    fn from(err: warp::Error) -> Self {
        ServerError::WarpError(err)
    }
}

impl From<reqwest::Error> for ServerError {
    fn from(err: reqwest::Error) -> Self {
        ServerError::ReqwestError(err)
    }
}

fn travel_error(w: &mut dyn Write, error: &dyn std::error::Error) {
    let mut cur_source = error.source();
    let mut index = 0;
    while let Some(source) = cur_source {
        writeln!(w, "{}: {}", index, source).unwrap();
        cur_source = source.source();
        index += 1;
    }
}
