use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const fn listen_on_localhost_default() -> bool {
    true
}

const fn port_default() -> u16 {
    2289
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default = "listen_on_localhost_default")]
    pub listen_on_localhost: bool,
    #[serde(default)]
    pub disable_ratelimits: bool,
    #[serde(default = "port_default")]
    pub port: u16,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    #[serde(default)]
    pub media: MediaConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            disable_ratelimits: false,
            listen_on_localhost: listen_on_localhost_default(),
            port: port_default(),
            tls: None,
            media: MediaConfig::default(),
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
