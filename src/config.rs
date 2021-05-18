use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub port: u16,
    pub tls: Option<TlsConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 2289,
            tls: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TlsConfig {
    pub key_file: PathBuf,
    pub cert_file: PathBuf,
}
