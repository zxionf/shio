//! HTTP 服务器、鉴权与 v1 API。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::config::Config;
use crate::db::Db;
use crate::error::RelayError;
use crate::store;
use crate::util;

/// 服务器共享状态。
#[derive(Clone)]
pub struct AppState {
    /// 配置。
    pub config: Arc<Config>,
    /// sqlite 连接。
    pub db: Db,
}

/// 构造路由。
pub fn router(state: AppState) -> Router {
    let max_body = usize::try_from(state.config.max_payload_bytes).unwrap_or(usize::MAX);
    Router::new()
        .route("/healthz", get(healthz))
        .route(
            "/v1/channels/{channel_id}/frames",
            post(post_frame).get(get_frames),
        )
        .route("/v1/channels/{channel_id}/ack", post(post_ack))
        .route("/v1/channels/{channel_id}/info", get(get_info))
        .layer(DefaultBodyLimit::max(max_body))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

#[derive(Debug, Clone)]
struct Auth {
    device: store::Device,
    channel: store::Channel,
    membership: store::Membership,
}

#[derive(Debug, Deserialize)]
struct FrameQuery {
    cursor: Option<i64>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct AckRequest {
    cursor: i64,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn unauthorized(code: &'static str) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, "invalid or missing token")
    }

    fn forbidden(code: &'static str) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, "not a member of this channel")
    }

    fn not_found(code: &'static str) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, "not found")
    }

    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    fn internal(err: impl std::fmt::Display) -> Self {
        tracing::error!(%err, "relay internal error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal server error",
        )
    }
}

impl From<RelayError> for ApiError {
    fn from(err: RelayError) -> Self {
        match err {
            RelayError::NotFound => Self::not_found("not_found"),
            RelayError::Conflict(message) => {
                let code = if message.contains("idempotency") {
                    "idempotency_conflict"
                } else {
                    "conflict"
                };
                Self::new(StatusCode::CONFLICT, code, message)
            }
            RelayError::PayloadTooLarge => Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "payload exceeds the channel limit",
            ),
            RelayError::Expired(message) => Self::new(StatusCode::GONE, "expired", message),
            RelayError::InvalidArgument(message) => Self::bad_request("invalid_argument", message),
            other => Self::internal(other),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error": self.code, "message": self.message})),
        )
            .into_response()
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn authenticate(state: &AppState, headers: &HeaderMap, channel_id: &str) -> Result<Auth, ApiError> {
    let token = bearer_token(headers).ok_or_else(|| ApiError::unauthorized("invalid_token"))?;
    let token_hash = util::sha256_hex(token.as_bytes());
    let device = state
        .db
        .with_conn(|conn| store::device_by_token_hash(conn, &token_hash))
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::unauthorized("invalid_token"))?;
    if device.revoked_at.is_some() {
        return Err(ApiError::unauthorized("invalid_token"));
    }
    let channel = state
        .db
        .with_conn(|conn| store::get_channel(conn, channel_id))
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found("channel_not_found"))?;
    let membership = state
        .db
        .with_conn(|conn| store::active_membership(conn, channel_id, &device.device_id))
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::forbidden("not_a_member"))?;
    if let Err(err) = state
        .db
        .with_conn(|conn| store::touch_device(conn, &device.device_id, util::now_secs()))
    {
        tracing::warn!(%err, "failed to update last_seen");
    }
    Ok(Auth {
        device,
        channel,
        membership,
    })
}

fn content_type(headers: &HeaderMap) -> Result<String, ApiError> {
    let value = match headers.get(CONTENT_TYPE) {
        Some(value) => value.to_str().map_err(|_| {
            ApiError::bad_request("invalid_content_type", "content type is not valid text")
        })?,
        None => "application/octet-stream",
    };
    validate_hint(value, "invalid_content_type")?;
    Ok(value.to_owned())
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, ApiError> {
    let value = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ApiError::bad_request("missing_idempotency_key", "Idempotency-Key is required")
        })?;
    validate_hint(value, "invalid_idempotency_key")?;
    Ok(value.to_owned())
}

fn validate_hint(value: &str, code: &'static str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
    {
        return Err(ApiError::bad_request(
            code,
            "value must be 1..=128 visible ASCII bytes",
        ));
    }
    Ok(())
}

async fn post_frame(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let auth = authenticate(&state, &headers, &channel_id)?;
    if body.is_empty() {
        return Err(ApiError::bad_request(
            "empty_payload",
            "payload must not be empty",
        ));
    }
    let content_type = content_type(&headers)?;
    let idem_key = idempotency_key(&headers)?;
    let max_payload = i64::try_from(state.config.max_payload_bytes)
        .map_err(|_| ApiError::internal("max_payload_bytes out of range"))?;
    let outcome = state
        .db
        .with_conn_mut(|conn| {
            store::append_frame(
                conn,
                &channel_id,
                &auth.device.device_id,
                &content_type,
                body.as_ref(),
                &idem_key,
                max_payload,
                util::now_secs(),
            )
        })
        .map_err(ApiError::from)?;
    Ok(Json(json!({"seq": outcome.seq})))
}

async fn get_frames(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<FrameQuery>,
) -> Result<Json<Value>, ApiError> {
    let auth = authenticate(&state, &headers, &channel_id)?;
    let cursor = query.cursor.unwrap_or(0);
    if cursor < 0 {
        return Err(ApiError::bad_request(
            "invalid_cursor",
            "cursor must not be negative",
        ));
    }
    let limit = query.limit.unwrap_or(100);
    if !(1..=1000).contains(&limit) {
        return Err(ApiError::bad_request(
            "invalid_limit",
            "limit must be within 1..=1000",
        ));
    }
    let page = state
        .db
        .with_conn(|conn| {
            store::fetch_frames(
                conn,
                &channel_id,
                cursor,
                limit,
                Some(&auth.device.device_id),
            )
        })
        .map_err(ApiError::from)?;
    let store::FramePage {
        frames,
        next_cursor,
        has_more,
        retained_from,
    } = page;
    let frames: Vec<Value> = frames
        .into_iter()
        .map(|frame| {
            json!({
                "seq": frame.seq,
                "sender": frame.sender,
                "content_type": frame.content_type,
                "payload_b64": BASE64.encode(frame.payload),
            })
        })
        .collect();
    Ok(Json(json!({
        "frames": frames,
        "next_cursor": next_cursor,
        "has_more": has_more,
        "retained_from": retained_from,
    })))
}

async fn post_ack(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<AckRequest>,
) -> Result<Json<Value>, ApiError> {
    let auth = authenticate(&state, &headers, &channel_id)?;
    if request.cursor < 0 || request.cursor >= auth.channel.next_seq {
        return Err(ApiError::bad_request(
            "invalid_cursor",
            "cursor must be within 0..next_seq",
        ));
    }
    let acked = state
        .db
        .with_conn(|conn| {
            store::ack_membership(conn, &channel_id, &auth.device.device_id, request.cursor)
        })
        .map_err(ApiError::from)?;
    Ok(Json(json!({"acked_cursor": acked})))
}

async fn get_info(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let auth = authenticate(&state, &headers, &channel_id)?;
    let (pending_frames, pending_bytes) = state
        .db
        .with_conn(|conn| {
            store::pending_stats(
                conn,
                &channel_id,
                &auth.device.device_id,
                auth.membership.acked_cursor,
            )
        })
        .map_err(ApiError::from)?;
    let frame_ttl_secs = auth
        .channel
        .frame_ttl_secs
        .unwrap_or_else(|| i64::try_from(state.config.frame_ttl_secs).unwrap_or(i64::MAX));
    let max_payload_bytes = auth
        .channel
        .max_payload_bytes
        .unwrap_or_else(|| i64::try_from(state.config.max_payload_bytes).unwrap_or(i64::MAX));
    Ok(Json(json!({
        "channel_id": channel_id,
        "device_id": auth.device.device_id,
        "acked_cursor": auth.membership.acked_cursor,
        "next_seq": auth.channel.next_seq,
        "pending_frames": pending_frames,
        "pending_bytes": pending_bytes,
        "quota_bytes": auth.channel.quota_bytes,
        "frame_ttl_secs": frame_ttl_secs,
        "retained_from": auth.channel.retained_from,
        "max_payload_bytes": max_payload_bytes,
        "relay_version": env!("CARGO_PKG_VERSION"),
    })))
}

/// 打开数据库并启动 HTTP 服务。
pub async fn serve(config: Config) -> crate::Result<()> {
    let db = Db::open(&config.data)?;
    let listen = config.listen.clone();
    let data = config.data.clone();
    let state = AppState {
        config: Arc::new(config),
        db,
    };
    let app = router(state);
    let listener = TcpListener::bind(&listen)
        .await
        .map_err(|err| RelayError::Config(format!("bind {listen}: {err}")))?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, data = %data.display(), "relay listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(RelayError::Io)?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(%err, "failed to install Ctrl-C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::error!(%err, "failed to install SIGTERM handler");
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            data: dir.path().join("relay.db"),
            ..Config::default()
        };
        let db = Db::open(&config.data).unwrap();
        let app = router(AppState {
            config: Arc::new(config),
            db,
        });
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(&body[..], b"ok");
    }
}
