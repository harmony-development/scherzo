#![feature(
    once_cell,
    let_else,
    const_intrinsic_copy,
    const_mut_refs,
    type_alias_impl_trait,
    drain_filter
)]
#![allow(clippy::unit_arg, clippy::blocks_in_if_conditions)]

use hrpc::exports::http;
use parking_lot::Mutex;
use triomphe::Arc;

pub mod config;
pub mod db;
pub mod error;
pub mod impls;
pub mod key;
pub mod utils;

pub use self::error::{rest_error_response, ServerError};

pub const SCHERZO_VERSION: &str = git_version::git_version!(
    prefix = "git:",
    cargo_prefix = "cargo:",
    fallback = "unknown"
);

pub type ServerResult<T> = Result<T, ServerError>;

pub type SharedConfig = Arc<Mutex<SharedConfigData>>;
#[derive(Default)]
pub struct SharedConfigData {
    pub motd: String,
}

pub use harmony_rust_sdk::api;
