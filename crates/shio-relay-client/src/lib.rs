//! shio relay 的 HTTP `RelayTransport` 实现。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod http;

pub use http::HttpRelayTransport;
