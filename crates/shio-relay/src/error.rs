//! 中继错误类型。

use std::io;

use thiserror::Error;

/// 中继错误。
#[derive(Debug, Error)]
pub enum RelayError {
    /// 配置读取或校验失败。
    #[error("config: {0}")]
    Config(String),
    /// IO 错误。
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// sqlite 错误。
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    /// TOML 解析错误。
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    /// JSON 编码/解码错误。
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// 参数非法。
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),
    /// 资源不存在。
    #[error("not found")]
    NotFound,
    /// 状态冲突。
    #[error("conflict: {0}")]
    Conflict(String),
    /// payload 超过上限。
    #[error("payload too large")]
    PayloadTooLarge,
    /// channel 配额或磁盘空间不足。
    #[error("storage full")]
    StorageFull,
    /// 凭据或入网码已过期。
    #[error("expired: {0}")]
    Expired(&'static str),
}

/// 中继结果。
pub type Result<T> = std::result::Result<T, RelayError>;
