use std::path::{Path, PathBuf};

use lettre::transport::smtp::authentication::Credentials;
use serde::{Deserialize, Serialize};

use crate::{ServerError, ServerResult};

const fn listen_on_localhost_default() -> bool {
    true
}

const fn port_default() -> u16 {
    2289
}

const fn db_cache_limit_default() -> u64 {
    1024
}

fn federation_config_default() -> Option<FederationConfig> {
    Some(FederationConfig::default())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub cors_dev: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub server_description: String,
    #[serde(default = "listen_on_localhost_default")]
    pub listen_on_localhost: bool,
    #[serde(default)]
    pub log_headers: bool,
    #[serde(default = "port_default")]
    pub port: u16,
    #[serde(default)]
    pub policy: PolicyConfig,
    #[serde(default)]
    pub db: DbConfig,
    #[serde(default)]
    pub media: MediaConfig,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    #[serde(default = "federation_config_default")]
    pub federation: Option<FederationConfig>,
    #[serde(default)]
    pub email: Option<EmailConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cors_dev: false,
            host: String::new(),
            server_description: String::new(),
            log_headers: false,
            listen_on_localhost: listen_on_localhost_default(),
            port: port_default(),
            policy: PolicyConfig::default(),
            db: DbConfig::default(),
            media: MediaConfig::default(),
            tls: None,
            federation: federation_config_default(),
            email: None,
        }
    }
}

const fn max_concurrent_requests_default() -> usize {
    512
}

#[derive(Default, Debug, Clone, Deserialize, Serialize)]
pub struct RateLimitConfig {
    #[serde(default)]
    pub disable: bool,
    #[serde(default)]
    pub client_ip_header_name: Option<String>,
    #[serde(default)]
    pub allowed_ips: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PolicyConfig {
    #[serde(default)]
    pub ratelimit: RateLimitConfig,
    #[serde(default)]
    pub disable_registration: bool,
    /// only takes effect if email config is set (duh)
    #[serde(default)]
    pub disable_registration_email_validation: bool,
    #[serde(default = "max_concurrent_requests_default")]
    pub max_concurrent_requests: usize,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            ratelimit: RateLimitConfig::default(),
            disable_registration: false,
            disable_registration_email_validation: false,
            max_concurrent_requests: max_concurrent_requests_default(),
        }
    }
}

const fn sled_load_to_cache_on_startup_default() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DbConfig {
    /// This is in MiB
    #[serde(default = "db_cache_limit_default")]
    pub db_cache_limit: u64,
    #[serde(default)]
    pub db_backup_path: Option<PathBuf>,
    #[serde(default)]
    pub sled_throughput_at_storage_cost: bool,
    #[serde(default = "sled_load_to_cache_on_startup_default")]
    pub sled_load_to_cache_on_startup: bool,
}

impl Default for DbConfig {
    fn default() -> Self {
        Self {
            db_cache_limit: db_cache_limit_default(),
            db_backup_path: None,
            sled_throughput_at_storage_cost: false,
            sled_load_to_cache_on_startup: sled_load_to_cache_on_startup_default(),
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
    50
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MediaConfig {
    #[serde(default = "media_root_default")]
    pub media_root: PathBuf,
    /// This is in MiB
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmailConfig {
    pub server: String,
    pub port: u16,
    pub from: String,
    credentials_file: Option<PathBuf>,
}

impl EmailConfig {
    pub async fn read_credentials(&self) -> Option<ServerResult<EmailCredentials>> {
        if let Some(path) = &self.credentials_file {
            Some(Self::read_credentials_inner(path).await)
        } else {
            None
        }
    }

    async fn read_credentials_inner(path: &Path) -> ServerResult<EmailCredentials> {
        let raw = tokio::fs::read(path).await?;
        let creds = toml::from_slice(&raw).map_err(ServerError::InvalidEmailConfig)?;
        Ok(creds)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmailCredentials {
    pub username: String,
    pub password: String,
}

impl From<EmailCredentials> for Credentials {
    fn from(creds: EmailCredentials) -> Self {
        Credentials::new(creds.username, creds.password)
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod test {
    use super::FederationConfig;

    #[test]
    fn host_allowed() {
        let mut fed_conf = FederationConfig::default();
        fed_conf.host_allow_list = vec!["test".to_string()];
        assert!(fed_conf.is_host_allowed("test").is_ok());
        assert!(fed_conf.is_host_allowed("not_test").is_err());
    }

    #[test]
    fn host_blocked() {
        let mut fed_conf = FederationConfig::default();
        fed_conf.host_block_list = vec!["test".to_string()];
        assert!(fed_conf.is_host_allowed("test").is_err());
        assert!(fed_conf.is_host_allowed("not_test").is_ok());
    }

    #[test]
    fn host_mixed() {
        let mut fed_conf = FederationConfig::default();
        fed_conf.host_block_list = vec!["test".to_string()];
        fed_conf.host_allow_list = vec!["hello".to_string()];
        assert!(fed_conf.is_host_allowed("test").is_err());
        assert!(fed_conf.is_host_allowed("not_test").is_err());
        assert!(fed_conf.is_host_allowed("hello").is_ok());
    }
}
