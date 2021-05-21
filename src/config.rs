use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub port: u16,
    pub tls: Option<TlsConfig>,
    pub media: MediaConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 2289,
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MediaConfig {
    pub media_root: PathBuf,
    pub max_upload_length: u64,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            media_root: Path::new("./media_root").to_path_buf(),
            max_upload_length: 1000 * 1000 * 50,
        }
    }
}
