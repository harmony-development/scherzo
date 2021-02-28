#![allow(clippy::unit_arg, clippy::blocks_in_if_conditions)]

use std::fmt::{self, Display, Formatter};

use harmony_rust_sdk::api::exports::hrpc::server::{json_err_bytes, CustomError, StatusCode};

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
    Unauthenticated,
    NotImplemented,
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
            ServerError::UserAlreadyExists => write!(f, "user already exists"),
            ServerError::Unauthenticated => write!(f, "invalid auth id"),
            ServerError::NotImplemented => write!(f, "not implemented"),
        }
    }
}

impl CustomError for ServerError {
    fn code(&self) -> StatusCode {
        match self {
            ServerError::InvalidAuthId
            | ServerError::NoFieldSpecified
            | ServerError::NoSuchField
            | ServerError::NoSuchChoice {
                choice: _,
                expected_any_of: _,
            }
            | ServerError::WrongStep {
                expected: _,
                got: _,
            }
            | ServerError::WrongTypeForField {
                name: _,
                expected: _,
            }
            | ServerError::WrongUserOrPassword { email: _ }
            | ServerError::UserAlreadyExists
            | ServerError::Unauthenticated => StatusCode::BAD_REQUEST,
            ServerError::NotImplemented => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> Vec<u8> {
        json_err_bytes(&self.to_string())
    }
}
