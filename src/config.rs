use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ServerError;

const fn listen_on_localhost_default() -> bool {
    true
}

const fn port_default() -> u16 {
    2289
}

const fn db_cache_limit_default() -> u64 {
    1024 * 1024 * 1024
}

fn federation_config_default() -> Option<FederationConfig> {
    Some(FederationConfig::default())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub server_description: String,
    #[serde(default = "listen_on_localhost_default")]
    pub listen_on_localhost: bool,
    #[serde(default)]
    pub disable_ratelimits: bool,
    #[serde(default = "port_default")]
    pub port: u16,
    #[serde(default = "db_cache_limit_default")]
    pub db_cache_limit: u64,
    #[serde(default)]
    pub db_backup_path: Option<PathBuf>,
    #[serde(default)]
    pub sled_throughput_at_storage_cost: bool,
    #[serde(default)]
    pub media: MediaConfig,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    #[serde(default = "federation_config_default")]
    pub federation: Option<FederationConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: String::new(),
            server_description: String::new(),
            disable_ratelimits: false,
            listen_on_localhost: listen_on_localhost_default(),
            port: port_default(),
            db_cache_limit: db_cache_limit_default(),
            db_backup_path: None,
            sled_throughput_at_storage_cost: false,
            media: MediaConfig::default(),
            tls: None,
            federation: federation_config_default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TlsConfig {
    pub key_file: PathBuf,
    pub cert_file: PathBuf,
}

fn media_root_default() -> PathBuf {
    Path::new("./media_root").to_path_buf()
}

const fn max_upload_length_default() -> u64 {
    1000 * 1000 * 50
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MediaConfig {
    #[serde(default = "media_root_default")]
    pub media_root: PathBuf,
    #[serde(default = "max_upload_length_default")]
    pub max_upload_length: u64,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            media_root: media_root_default(),
            max_upload_length: max_upload_length_default(),
        }
    }
}

fn federation_key_default() -> PathBuf {
    Path::new("./federation_key").to_path_buf()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FederationConfig {
    #[serde(default = "federation_key_default")]
    pub key: PathBuf,
    #[serde(default)]
    pub host_allow_list: Vec<String>,
    #[serde(default)]
    pub host_block_list: Vec<String>,
}

impl FederationConfig {
    pub fn is_host_allowed(&self, host: &str) -> Result<(), ServerError> {
        (self.host_allow_list.iter().any(|oh| oh.eq(host))
            || (self.host_allow_list.is_empty()
                && !self.host_block_list.iter().any(|oh| oh.eq(host))))
        .then(|| ())
        .ok_or(ServerError::HostNotAllowed)
    }
}

impl Default for FederationConfig {
    fn default() -> Self {
        Self {
            key: federation_key_default(),
            host_allow_list: Vec::new(),
            host_block_list: Vec::new(),
        }
    }
}
