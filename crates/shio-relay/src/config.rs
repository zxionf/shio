//! `relay.toml` 配置。

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{RelayError, Result};

/// 默认监听地址。
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8080";
/// 默认数据库路径。
pub const DEFAULT_DATA: &str = "/var/lib/shio-relay/relay.db";
/// 默认帧保留时间：30 天。
pub const DEFAULT_FRAME_TTL_SECS: u64 = 30 * 24 * 60 * 60;
/// 默认 channel 配额：100 MB。
pub const DEFAULT_CHANNEL_QUOTA_BYTES: u64 = 100 * 1024 * 1024;
/// 默认单帧上限：8 MiB。
pub const DEFAULT_MAX_PAYLOAD_BYTES: u64 = 8 * 1024 * 1024;
/// 默认每 token 每分钟请求数。
pub const DEFAULT_RATE_LIMIT_PER_MIN: u32 = 60;

/// 中继配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// 监听地址。
    pub listen: String,
    /// sqlite 文件路径。
    pub data: PathBuf,
    /// 帧保留时间（秒）。
    pub frame_ttl_secs: u64,
    /// 每 channel 默认配额（字节）。
    pub channel_quota_bytes: u64,
    /// 单帧默认上限（字节）。
    pub max_payload_bytes: u64,
    /// 每 token 每分钟请求数。
    pub rate_limit_per_min: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: DEFAULT_LISTEN.to_owned(),
            data: PathBuf::from(DEFAULT_DATA),
            frame_ttl_secs: DEFAULT_FRAME_TTL_SECS,
            channel_quota_bytes: DEFAULT_CHANNEL_QUOTA_BYTES,
            max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
            rate_limit_per_min: DEFAULT_RATE_LIMIT_PER_MIN,
        }
    }
}

impl Config {
    /// 从 TOML 文件读取并校验。
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = fs::read_to_string(path)
            .map_err(|err| RelayError::Config(format!("read {}: {err}", path.display())))?;
        let config: Self = toml::from_str(&text)?;
        config.validate()?;
        Ok(config)
    }

    /// 校验配置范围。
    pub fn validate(&self) -> Result<()> {
        if self.listen.trim().is_empty() {
            return Err(RelayError::Config("listen must not be empty".to_owned()));
        }
        self.listen.parse::<SocketAddr>().map_err(|err| {
            RelayError::Config(format!(
                "listen {} is not a socket address: {err}",
                self.listen
            ))
        })?;
        if self.data.as_os_str().is_empty() {
            return Err(RelayError::Config("data must not be empty".to_owned()));
        }
        if self.frame_ttl_secs == 0 {
            return Err(RelayError::Config(
                "frame_ttl_secs must be greater than zero".to_owned(),
            ));
        }
        if self.channel_quota_bytes == 0 {
            return Err(RelayError::Config(
                "channel_quota_bytes must be greater than zero".to_owned(),
            ));
        }
        if self.max_payload_bytes == 0 {
            return Err(RelayError::Config(
                "max_payload_bytes must be greater than zero".to_owned(),
            ));
        }
        if self.max_payload_bytes > self.channel_quota_bytes {
            return Err(RelayError::Config(
                "max_payload_bytes must not exceed channel_quota_bytes".to_owned(),
            ));
        }
        if self.rate_limit_per_min == 0 {
            return Err(RelayError::Config(
                "rate_limit_per_min must be greater than zero".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn parses_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.toml");
        std::fs::write(
            &path,
            r#"
listen = "127.0.0.1:0"
data = "relay.db"
frame_ttl_secs = 60
channel_quota_bytes = 4096
max_payload_bytes = 1024
rate_limit_per_min = 7
"#,
        )
        .unwrap();

        let config = Config::load(&path).unwrap();
        assert_eq!(config.listen, "127.0.0.1:0");
        assert_eq!(config.data, PathBuf::from("relay.db"));
        assert_eq!(config.frame_ttl_secs, 60);
        assert_eq!(config.channel_quota_bytes, 4096);
        assert_eq!(config.max_payload_bytes, 1024);
        assert_eq!(config.rate_limit_per_min, 7);
    }

    #[test]
    fn rejects_bad_values() {
        let config = Config {
            listen: "not-an-address".to_owned(),
            ..Config::default()
        };
        assert!(config.validate().is_err());

        let config = Config {
            max_payload_bytes: 0,
            ..Config::default()
        };
        assert!(config.validate().is_err());

        let quota = Config::default().channel_quota_bytes;
        let config = Config {
            max_payload_bytes: quota + 1,
            ..Config::default()
        };
        assert!(config.validate().is_err());

        let config = Config {
            rate_limit_per_min: 0,
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.toml");
        std::fs::write(&path, "unknown = 1\n").unwrap();
        assert!(Config::load(&path).is_err());
    }
}
