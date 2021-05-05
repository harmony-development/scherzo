#![allow(clippy::unit_arg, clippy::blocks_in_if_conditions)]

use std::fmt::{self, Display, Formatter};

use harmony_rust_sdk::api::exports::hrpc::server::{CustomError, StatusCode};

pub mod db;
pub mod impls;

#[derive(Debug)]
pub enum ServerError {
    InvalidAuthId,
    NoFieldSpecified,
    NoSuchField,
    NoSuchChoice {
        choice: String,
        expected_any_of: Vec<String>,
    },
    WrongStep {
        expected: String,
        got: String,
    },
    WrongTypeForField {
        name: String,
        expected: String,
    },
    WrongUserOrPassword {
        email: String,
    },
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
    NoSuchGuild(u64),
    NoSuchInvite(String),
    NoSuchUser(u64),
    InternalServerError,
    SessionExpired,
    NotEnoughPermissions {
        must_be_guild_owner: bool,
        missing_permissions: Vec<String>,
    },
}

impl Display for ServerError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
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
            ServerError::UserNotInGuild { guild_id, user_id } => {
                write!(f, "user {} not in guild {}", user_id, guild_id)
            }
            ServerError::UserAlreadyExists => write!(f, "user already exists"),
            ServerError::Unauthenticated => write!(f, "invalid-session"),
            ServerError::NotImplemented => write!(f, "not implemented"),
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
                missing_permissions,
            } => {
                writeln!(f, "missing permissions: ")?;
                if *must_be_guild_owner {
                    writeln!(f, "must be guild owner")?;
                }
                for perm in missing_permissions {
                    writeln!(f, "{}", perm)?;
                }
                Ok(())
            }
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
            | ServerError::SessionExpired => StatusCode::BAD_REQUEST,
            ServerError::NotImplemented | ServerError::InternalServerError => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    fn message(&self) -> Vec<u8> {
        self.to_string().into_bytes()
    }
}
