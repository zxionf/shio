//! `reqwest::blocking` 实现的 HTTP relay transport。
//!
//! 只负责 HTTP 与 JSON 传输；帧语义、`content_type` 分派和 gap 判断在
//! `shio_sync::relay::RelayClient`。

use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use reqwest::blocking::{Client, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use shio_sync::relay::{RelayError, RelayFrame, RelayInfo, RelayPage, RelayResult, RelayTransport};
use shio_sync::DeviceId;

/// 基于 HTTP 的 relay transport。
#[derive(Clone)]
pub struct HttpRelayTransport {
    base: String,
    token: String,
    client: Client,
}

impl HttpRelayTransport {
    /// 创建 transport；`base_url` 例如 `https://relay.example.com`。
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> RelayResult<Self> {
        let base = base_url.into();
        let token = token.into();
        if base.trim().is_empty() {
            return Err(RelayError::Invalid("base URL is empty".to_owned()));
        }
        if token.is_empty() {
            return Err(RelayError::Invalid("token is empty".to_owned()));
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(network)?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            token,
            client,
        })
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }

    fn url(&self, channel_id: &str, suffix: &str) -> String {
        format!("{}/v1/channels/{channel_id}{suffix}", self.base)
    }
}

impl RelayTransport for HttpRelayTransport {
    fn push(
        &mut self,
        channel_id: &str,
        content_type: &str,
        payload: &[u8],
        idempotency_key: &str,
    ) -> RelayResult<i64> {
        let response = self
            .client
            .post(self.url(channel_id, "/frames"))
            .header("authorization", self.bearer())
            .header("idempotency-key", idempotency_key)
            .header("content-type", content_type)
            .body(payload.to_vec())
            .send()
            .map_err(network)?;
        let body: PushResponse = parse(check(response)?)?;
        Ok(body.seq)
    }

    fn pull(&mut self, channel_id: &str, cursor: i64, limit: usize) -> RelayResult<RelayPage> {
        let response = self
            .client
            .get(self.url(channel_id, "/frames"))
            .query(&[("cursor", cursor.to_string()), ("limit", limit.to_string())])
            .header("authorization", self.bearer())
            .send()
            .map_err(network)?;
        let body: PullResponse = parse(check(response)?)?;
        let mut frames = Vec::with_capacity(body.frames.len());
        for frame in body.frames {
            frames.push(RelayFrame {
                seq: frame.seq,
                sender: parse_device_id(&frame.sender)?,
                content_type: frame.content_type,
                payload: BASE64.decode(frame.payload_b64).map_err(|err| {
                    RelayError::Invalid(format!("payload base64 is invalid: {err}"))
                })?,
            });
        }
        Ok(RelayPage {
            frames,
            next_cursor: body.next_cursor,
            has_more: body.has_more,
            retained_from: body.retained_from,
        })
    }

    fn ack(&mut self, channel_id: &str, cursor: i64) -> RelayResult<i64> {
        let response = self
            .client
            .post(self.url(channel_id, "/ack"))
            .header("authorization", self.bearer())
            .json(&AckRequest { cursor })
            .send()
            .map_err(network)?;
        let body: AckResponse = parse(check(response)?)?;
        Ok(body.acked_cursor)
    }

    fn info(&mut self, channel_id: &str) -> RelayResult<RelayInfo> {
        let response = self
            .client
            .get(self.url(channel_id, "/info"))
            .header("authorization", self.bearer())
            .send()
            .map_err(network)?;
        let body: InfoResponse = parse(check(response)?)?;
        Ok(RelayInfo {
            channel_id: body.channel_id,
            device_id: body.device_id,
            acked_cursor: body.acked_cursor,
            next_seq: body.next_seq,
            pending_frames: body.pending_frames,
            pending_bytes: body.pending_bytes,
            quota_bytes: body.quota_bytes,
            frame_ttl_secs: body.frame_ttl_secs,
            retained_from: body.retained_from,
            max_payload_bytes: body.max_payload_bytes,
        })
    }
}

#[derive(Debug, Deserialize)]
struct PushResponse {
    seq: i64,
}

#[derive(Debug, Deserialize)]
struct PullResponse {
    frames: Vec<FrameResponse>,
    next_cursor: i64,
    has_more: bool,
    retained_from: i64,
}

#[derive(Debug, Deserialize)]
struct FrameResponse {
    seq: i64,
    sender: String,
    content_type: String,
    payload_b64: String,
}

#[derive(Debug, Serialize)]
struct AckRequest {
    cursor: i64,
}

#[derive(Debug, Deserialize)]
struct AckResponse {
    acked_cursor: i64,
}

#[derive(Debug, Deserialize)]
struct InfoResponse {
    channel_id: String,
    device_id: String,
    acked_cursor: i64,
    next_seq: i64,
    pending_frames: i64,
    pending_bytes: i64,
    quota_bytes: i64,
    frame_ttl_secs: i64,
    retained_from: i64,
    max_payload_bytes: i64,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    error: String,
    message: String,
}

fn network(err: reqwest::Error) -> RelayError {
    RelayError::Network(err.to_string())
}

fn parse<T: DeserializeOwned>(response: Response) -> RelayResult<T> {
    response
        .json()
        .map_err(|err| RelayError::Invalid(err.to_string()))
}

fn check(response: Response) -> RelayResult<Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.bytes().map_err(network)?;
    let (code, message) = serde_json::from_slice::<ErrorBody>(&body)
        .map(|body| (body.error, body.message))
        .unwrap_or_else(|_| {
            (
                "http_error".to_owned(),
                String::from_utf8_lossy(&body).into_owned(),
            )
        });
    Err(RelayError::Http {
        status: status.as_u16(),
        code,
        message,
    })
}

fn parse_device_id(value: &str) -> RelayResult<DeviceId> {
    let bytes = value.as_bytes();
    if bytes.len() != 32 {
        return Err(RelayError::Invalid(format!(
            "device id must be 32 hex characters, got {}",
            value.len()
        )));
    }
    let mut out = [0u8; 16];
    for (index, byte) in out.iter_mut().enumerate() {
        let hi = hex_nibble(bytes[index * 2])?;
        let lo = hex_nibble(bytes[index * 2 + 1])?;
        *byte = (hi << 4) | lo;
    }
    Ok(DeviceId::from_bytes(out))
}

fn hex_nibble(byte: u8) -> RelayResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(RelayError::Invalid(
            "device id contains a non-hex character".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_ids() {
        let id = parse_device_id("000102030405060708090a0b0c0d0e0f").unwrap();
        assert_eq!(id.to_string(), "000102030405060708090a0b0c0d0e0f");
        assert!(parse_device_id("not hex").is_err());
    }

    #[test]
    fn rejects_empty_inputs() {
        assert!(HttpRelayTransport::new("", "token").is_err());
        assert!(HttpRelayTransport::new("http://localhost", "").is_err());
    }
}
