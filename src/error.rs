use std::{
    error::Error as StdError,
    fmt::{self, Display, Formatter, Write},
    io::Error as IoError,
    str::FromStr,
    time::Duration,
};

use crate::api::{
    exports::hrpc::{
        exports::http::{self, uri::InvalidUri as UrlParseError, StatusCode},
        server::error::HrpcError,
    },
    HomeserverIdParseError,
};
use hrpc::{
    body::Body,
    common::transport::http::{content_header_value, version_header_name, version_header_value},
    decode::DecodeBodyError,
    encode::encode_protobuf_message,
    proto::HrpcErrorIdentifier,
    server::transport::http::{box_body, HttpResponse},
};
use hyper::{http::HeaderValue, Uri};
use smol_str::SmolStr;

use crate::db::DbError;

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
    WrongEmailOrPassword {
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
    UnderSpecifiedChannels,
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
    DbError(DbError),
    IoError(IoError),
    InvalidFileId,
    HttpError(hyper::Error),
    FileExtractUnexpected(SmolStr),
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
    LinkNotFound(Uri),
    MessageContentCantBeEmpty,
    InviteExists(String),
    InviteNameEmpty,
    NotMedia,
    InvalidAgainst(HomeserverIdParseError),
    CantKickOrBanYourself,
    TooManyBatchedRequests,
    InvalidBatchEndpoint(String),
    InvalidRegistrationToken,
    WebRTCError(anyhow::Error),
    MultipartError(multer::Error),
    MustNotBeLastOwner,
    ContentCantBeSentByUser,
    InvalidProtoMessage(DecodeBodyError),
    InvalidEmailConfig(toml::de::Error),
    FailedToFetchLink(reqwest::Error),
    FailedToDownload(reqwest::Error),
}

impl StdError for ServerError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            ServerError::InvalidProtoMessage(err) => Some(err),
            ServerError::InvalidAgainst(err) => Some(err),
            ServerError::InvalidUrl(err) => Some(err),
            ServerError::IoError(err) => Some(err),
            ServerError::HttpError(err) => Some(err),
            ServerError::WebRTCError(err) => err.source(),
            ServerError::DbError(err) => Some(err),
            ServerError::MultipartError(err) => Some(err),
            ServerError::FailedToFetchLink(err) => Some(err),
            ServerError::FailedToDownload(err) => Some(err),
            _ => None,
        }
    }
}

impl Display for ServerError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            ServerError::InvalidEmailConfig(err) => write!(f, "invalid email config: {}", err),
            ServerError::InvalidProtoMessage(err) => {
                write!(f, "couldn't decode a response body: {}", err)
            }
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
            ServerError::WrongEmailOrPassword { email } => {
                write!(f, "invalid credentials for email {}", email)
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
            ServerError::UnderSpecifiedChannels => write!(f, "not all required channels specified"),
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
            ServerError::IoError(err) => {
                travel_error(f, err);
                write!(f, "io error occured: {}", err)
            }
            ServerError::HttpError(err) => {
                travel_error(f, err);
                write!(f, "error occured in reqwest: {}", err)
            }
            ServerError::FileExtractUnexpected(msg) => write!(f, "unexpected behaviour: {}", msg),
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
            ServerError::InvalidAgainst(err) => write!(f, "malformed Against header: {}", err),
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
            ServerError::InviteExists(name) => write!(f, "invite {} already exists", name),
            ServerError::InviteNameEmpty => write!(f, "invite name can't be empty"),
            ServerError::NotMedia => write!(f, "the requested URL does not point to media"),
            ServerError::CantKickOrBanYourself => write!(f, "you can't ban or kick yourself"),
            ServerError::InvalidBatchEndpoint(endpoint) => {
                write!(f, "can't use this endpoint in batch: {}", endpoint)
            }
            ServerError::TooManyBatchedRequests => {
                write!(f, "too many requests in one batch (cannot exceed 64)")
            }
            ServerError::InvalidRegistrationToken => write!(f, "invalid registration token"),
            ServerError::WebRTCError(err) => write!(f, "webrtc error: {}", err),
            ServerError::DbError(err) => write!(f, "database error: {}", err),
            ServerError::MultipartError(err) => write!(f, "multipart error: {}", err),
            ServerError::MustNotBeLastOwner => {
                f.write_str("must not be the last owner left in the guild")
            }
            ServerError::ContentCantBeSentByUser => {
                f.write_str("this content type cannot be used by a regular user")
            }
            ServerError::FailedToFetchLink(err) => {
                write!(f, "failed to fetch link: {}", err)
            }
            ServerError::FailedToDownload(err) => {
                write!(f, "failed to download: {}", err)
            }
        }
    }
}

impl ServerError {
    pub const fn status(&self) -> StatusCode {
        match self {
            ServerError::InvalidAuthId
            | ServerError::NoFieldSpecified
            | ServerError::NoSuchField
            | ServerError::NoSuchChoice { .. }
            | ServerError::WrongStep { .. }
            | ServerError::WrongTypeForField { .. }
            | ServerError::WrongEmailOrPassword { .. }
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
            | ServerError::MessageContentCantBeEmpty
            | ServerError::InviteExists(_)
            | ServerError::InviteNameEmpty
            | ServerError::UnderSpecifiedChannels
            | ServerError::NotMedia
            | ServerError::InvalidAgainst(_)
            | ServerError::CantKickOrBanYourself
            | ServerError::InvalidBatchEndpoint(_)
            | ServerError::TooManyBatchedRequests
            | ServerError::InvalidRegistrationToken
            | ServerError::MultipartError(
                multer::Error::StreamSizeExceeded { .. } | multer::Error::UnknownField { .. },
            )
            | ServerError::MustNotBeLastOwner
            | ServerError::ContentCantBeSentByUser
            | ServerError::InvalidProtoMessage(_) => StatusCode::BAD_REQUEST,
            ServerError::FederationDisabled | ServerError::HostNotAllowed => StatusCode::FORBIDDEN,
            ServerError::IoError(_)
            | ServerError::InternalServerError
            | ServerError::HttpError(_)
            | ServerError::FileExtractUnexpected(_)
            | ServerError::CantGetKey
            | ServerError::CantGetHostKey(_)
            | ServerError::WebRTCError(_)
            | ServerError::DbError(_)
            | ServerError::MultipartError(_)
            | ServerError::InvalidEmailConfig(_)
            | ServerError::FailedToFetchLink(_)
            | ServerError::FailedToDownload(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ServerError::TooFast(_) => StatusCode::TOO_MANY_REQUESTS,
            ServerError::MediaNotFound | ServerError::LinkNotFound(_) => StatusCode::NOT_FOUND,
            ServerError::NotImplemented => StatusCode::NOT_IMPLEMENTED,
        }
    }

    pub const fn identifier(&self) -> &'static str {
        match self {
            ServerError::InternalServerError
            | ServerError::IoError(_)
            | ServerError::HttpError(_)
            | ServerError::FileExtractUnexpected(_)
            | ServerError::CantGetKey
            | ServerError::CantGetHostKey(_)
            | ServerError::WebRTCError(_)
            | ServerError::DbError(_)
            | ServerError::MultipartError(_)
            | ServerError::InvalidProtoMessage(_)
            | ServerError::InvalidEmailConfig(_)
            | ServerError::FailedToFetchLink(_)
            | ServerError::FailedToDownload(_) => HrpcErrorIdentifier::InternalServerError.as_id(),
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
            ServerError::WrongEmailOrPassword { .. } => "h.bad-password\nh.bad-email",
            ServerError::UserNotInGuild { .. } => "h.not-joined",
            ServerError::NotImplemented => HrpcErrorIdentifier::NotImplemented.as_id(),
            ServerError::ChannelAlreadyExists { .. } => "h.channel-already-exists",
            ServerError::GuildAlreadyExists(_) => "h.guild-already-exists",
            ServerError::NoSuchMessage { .. } => "h.bad-message-id",
            ServerError::NoSuchChannel { .. } => "h.bad-channel-id",
            ServerError::UnderSpecifiedChannels => "h.underspecified-channels",
            ServerError::NoSuchGuild(_) => "h.bad-guild-id",
            ServerError::NoSuchInvite(_) | ServerError::InviteNameEmpty => "h.bad-invite-id",
            ServerError::NoSuchUser(_) => "h.bad-user-id",
            ServerError::SessionExpired => "h.bad-session",
            ServerError::EmptyPermissionQuery => "h.permission-query-empty",
            ServerError::NoSuchRole { .. } => "h.bad-role-id",
            ServerError::NoRoleSpecified => "h.missing-role",
            ServerError::NoPermissionsSpecified => "h.missing-permissions",
            ServerError::InvalidFileId => "h.bad-file-id",
            ServerError::TooManyFiles => "too-many-files",
            ServerError::MissingFiles => "missing-files",
            ServerError::TooFast(_) => HrpcErrorIdentifier::ResourceExhausted.as_id(),
            ServerError::NotAnImage => "h.not-an-image",
            ServerError::NotMedia => "not-media",
            ServerError::MediaNotFound | ServerError::LinkNotFound(_) => {
                HrpcErrorIdentifier::NotFound.as_id()
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
            ServerError::InviteExists(_) => "h.invite-exists",
            ServerError::InvalidAgainst(_) => "h.invalid-against",
            ServerError::CantKickOrBanYourself => "h.cant-ban-kick-self",
            ServerError::InvalidBatchEndpoint(_) => "h.invalid-batch",
            ServerError::TooManyBatchedRequests => "h.too-many-batches",
            ServerError::InvalidRegistrationToken => "h.invalid-registration-token",
            ServerError::MustNotBeLastOwner => "h.last-owner-in-guild",
            ServerError::ContentCantBeSentByUser => "h.content-not-allowed-for-user",
        }
    }

    pub fn identifier_to_status(id: &str) -> Option<StatusCode> {
        if HrpcErrorIdentifier::from_str(id).is_ok() {
            return None;
        }

        match id {
            "h.federation-disabled" | "h.host-not-allowed" => Some(StatusCode::FORBIDDEN),
            _ => Some(StatusCode::BAD_REQUEST),
        }
    }

    pub fn into_http_response(self) -> HttpResponse {
        let status = self.status();
        let err = HrpcError::from(self);

        http::Response::builder()
            .status(status)
            .header(version_header_name(), version_header_value())
            .header(http::header::CONTENT_TYPE, content_header_value())
            .body(box_body(Body::full(encode_protobuf_message(&err).freeze())))
            .unwrap()
    }

    pub fn into_rest_http_response(self) -> HttpResponse {
        let status = self.status();
        let msg = self.to_string();

        rest_error_response(msg, status)
    }
}

pub fn rest_error_response(mut msg: String, status: StatusCode) -> HttpResponse {
    msg.insert_str(0, "{ message: \"");
    msg.push_str("\" }");

    http::Response::builder()
        .status(status)
        .header(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/json"),
        )
        .body(box_body(Body::full(msg.into_bytes())))
        .unwrap()
}

impl From<ServerError> for HrpcError {
    fn from(err: ServerError) -> Self {
        HrpcError::default()
            .with_identifier(err.identifier())
            .with_message(err.to_string())
    }
}

impl From<multer::Error> for ServerError {
    fn from(err: multer::Error) -> Self {
        ServerError::MultipartError(err)
    }
}

impl From<IoError> for ServerError {
    fn from(err: IoError) -> Self {
        ServerError::IoError(err)
    }
}

impl From<hyper::Error> for ServerError {
    fn from(err: hyper::Error) -> Self {
        ServerError::HttpError(err)
    }
}

impl From<DbError> for ServerError {
    fn from(err: DbError) -> Self {
        ServerError::DbError(err)
    }
}

impl From<DecodeBodyError> for ServerError {
    fn from(err: DecodeBodyError) -> Self {
        ServerError::InvalidProtoMessage(err)
    }
}

pub fn travel_error(w: &mut dyn Write, error: &dyn std::error::Error) {
    let mut cur_source = error.source();
    let mut index = 0;
    while let Some(source) = cur_source {
        writeln!(w, "{}: {}", index, source).unwrap();
        cur_source = source.source();
        index += 1;
    }
}
