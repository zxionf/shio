//! 内容无关的同步中继。
//!
//! 中继只维护 channel 的 payload 日志、成员游标与配额；不解析 payload。
//! 规格见 `docs/relay.md` v0.2。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod config;
pub mod db;
pub mod error;
pub mod server;

pub use config::Config;
pub use error::{RelayError, Result};
