//! HTTP API 集成测试。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::Value;
use shio_relay::db::Db;
use shio_relay::server::{router, AppState};
use shio_relay::{store, util, Config};
use tower::ServiceExt;

struct Ctx {
    _dir: tempfile::TempDir,
    app: Router,
    db: Db,
    channel: String,
    token_a: String,
    token_b: String,
    device_a: String,
    device_b: String,
}

fn auth(token: &str) -> String {
    format!("Bearer {token}")
}

async fn setup() -> Ctx {
    setup_with_rate(60).await
}

async fn setup_with_rate(rate_limit_per_min: u32) -> Ctx {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        data: dir.path().join("relay.db"),
        max_payload_bytes: 1024,
        channel_quota_bytes: 4096,
        rate_limit_per_min,
        ..Config::default()
    };
    let db = Db::open(&config.data).unwrap();
    let channel = "c".repeat(32);
    let device_a = "a".repeat(32);
    let device_b = "b".repeat(32);
    let token_a = "token-a".to_owned();
    let token_b = "token-b".to_owned();

    db.with_conn(|conn| store::insert_channel(conn, &channel, 4096, Some(3600), Some(1024), 1_000))
        .unwrap();
    db.with_conn(|conn| {
        store::insert_device(
            conn,
            &device_a,
            &util::sha256_hex(token_a.as_bytes()),
            1_000,
        )
    })
    .unwrap();
    db.with_conn(|conn| {
        store::insert_device(
            conn,
            &device_b,
            &util::sha256_hex(token_b.as_bytes()),
            1_000,
        )
    })
    .unwrap();
    db.with_conn(|conn| store::upsert_membership(conn, &channel, &device_a, 1_000))
        .unwrap();
    db.with_conn(|conn| store::upsert_membership(conn, &channel, &device_b, 1_000))
        .unwrap();

    let app = router(AppState::new(config, db.clone()));
    Ctx {
        _dir: dir,
        app,
        db,
        channel,
        token_a,
        token_b,
        device_a,
        device_b,
    }
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn post_frame_req(
    ctx: &Ctx,
    token: &str,
    idem: &str,
    content_type: &str,
    body: &[u8],
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/v1/channels/{}/frames", ctx.channel))
        .header("authorization", auth(token))
        .header("idempotency-key", idem)
        .header("content-type", content_type)
        .body(Body::from(body.to_vec()))
        .unwrap()
}

fn get_frames_req(ctx: &Ctx, token: &str, query: &str) -> Request<Body> {
    Request::builder()
        .uri(format!("/v1/channels/{}/frames?{query}", ctx.channel))
        .header("authorization", auth(token))
        .body(Body::empty())
        .unwrap()
}

fn post_json(uri: String, token: Option<&str>, body: String) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", auth(token));
    }
    builder.body(Body::from(body)).unwrap()
}

#[tokio::test]
async fn post_is_idempotent_and_get_excludes_self() {
    let ctx = setup().await;
    let (status, body) = send(
        &ctx.app,
        post_frame_req(&ctx, &ctx.token_a, "k1", "text/plain", b"hello"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["seq"].as_i64(), Some(1));

    let (status, body) = send(
        &ctx.app,
        post_frame_req(&ctx, &ctx.token_a, "k1", "text/plain", b"hello"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["seq"].as_i64(), Some(1));

    let (status, body) = send(
        &ctx.app,
        post_frame_req(&ctx, &ctx.token_a, "k1", "text/plain", b"other"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "idempotency_conflict");

    // 自己的帧默认不返回。
    let (status, body) = send(
        &ctx.app,
        get_frames_req(&ctx, &ctx.token_a, "cursor=0&limit=10"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["frames"].as_array().unwrap().len(), 0);
    assert_eq!(body["next_cursor"].as_i64(), Some(0));

    // B 直接写一帧，A 应能拉到。
    ctx.db
        .with_conn_mut(|conn| {
            store::append_frame(
                conn,
                &ctx.channel,
                &ctx.device_b,
                "text/plain",
                b"from-b",
                "seed-b",
                1024,
                2_000,
            )
        })
        .unwrap();
    let (status, body) = send(
        &ctx.app,
        get_frames_req(&ctx, &ctx.token_a, "cursor=0&limit=10"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let frames = body["frames"].as_array().unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["sender"].as_str(), Some(ctx.device_b.as_str()));
    assert_eq!(frames[0]["payload_b64"], "ZnJvbS1i");
    assert_eq!(body["next_cursor"].as_i64(), Some(2));
    assert_eq!(body["retained_from"].as_i64(), Some(1));
}

#[tokio::test]
async fn ack_is_forward_only_and_info_reports_state() {
    let ctx = setup().await;
    let (status, _) = send(
        &ctx.app,
        post_frame_req(&ctx, &ctx.token_a, "k1", "text/plain", b"hello"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let ack = |cursor: i64| {
        Request::builder()
            .method("POST")
            .uri(format!("/v1/channels/{}/ack", ctx.channel))
            .header("authorization", auth(&ctx.token_a))
            .header("content-type", "application/json")
            .body(Body::from(format!("{{\"cursor\":{cursor}}}")))
            .unwrap()
    };

    let (status, body) = send(&ctx.app, ack(1)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["acked_cursor"].as_i64(), Some(1));

    let (status, body) = send(&ctx.app, ack(0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["acked_cursor"].as_i64(), Some(1));

    let (status, body) = send(&ctx.app, ack(2)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_cursor");

    let info = Request::builder()
        .uri(format!("/v1/channels/{}/info", ctx.channel))
        .header("authorization", auth(&ctx.token_a))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&ctx.app, info).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["channel_id"].as_str(), Some(ctx.channel.as_str()));
    assert_eq!(body["device_id"].as_str(), Some(ctx.device_a.as_str()));
    assert_eq!(body["acked_cursor"].as_i64(), Some(1));
    assert_eq!(body["next_seq"].as_i64(), Some(2));
    assert_eq!(body["pending_frames"].as_i64(), Some(0));
    assert_eq!(body["pending_bytes"].as_i64(), Some(0));
    assert_eq!(body["quota_bytes"].as_i64(), Some(4096));
    assert_eq!(body["frame_ttl_secs"].as_i64(), Some(3600));
    assert_eq!(body["retained_from"].as_i64(), Some(1));
    assert_eq!(body["max_payload_bytes"].as_i64(), Some(1024));
    assert!(body["relay_version"].as_str().is_some());
}

#[tokio::test]
async fn auth_and_validation_errors() {
    let ctx = setup().await;

    // 无 token。
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/channels/{}/frames", ctx.channel))
        .header("idempotency-key", "k")
        .body(Body::from("x"))
        .unwrap();
    let (status, body) = send(&ctx.app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "invalid_token");

    // 未知 token。
    let mut req = post_frame_req(&ctx, "nope", "k", "text/plain", b"x");
    req.headers_mut()
        .insert("authorization", auth("nope").parse().unwrap());
    let (status, body) = send(&ctx.app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "invalid_token");

    // 非成员。
    let device_c = "e".repeat(32);
    let token_c = "token-c";
    ctx.db
        .with_conn(|conn| {
            store::insert_device(
                conn,
                &device_c,
                &util::sha256_hex(token_c.as_bytes()),
                1_000,
            )
        })
        .unwrap();
    let (status, body) = send(
        &ctx.app,
        post_frame_req(&ctx, token_c, "k", "text/plain", b"x"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "not_a_member");

    // channel 不存在。
    let req = Request::builder()
        .method("POST")
        .uri("/v1/channels/missing/frames")
        .header("authorization", auth(&ctx.token_a))
        .header("idempotency-key", "k")
        .body(Body::from("x"))
        .unwrap();
    let (status, body) = send(&ctx.app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "channel_not_found");

    // 全局撤销的 token。
    let device_d = "f".repeat(32);
    let token_d = "token-d";
    ctx.db
        .with_conn(|conn| {
            store::insert_device(
                conn,
                &device_d,
                &util::sha256_hex(token_d.as_bytes()),
                1_000,
            )
        })
        .unwrap();
    ctx.db
        .with_conn(|conn| store::upsert_membership(conn, &ctx.channel, &device_d, 1_000))
        .unwrap();
    ctx.db
        .with_conn(|conn| store::revoke_device(conn, &device_d, 2_000))
        .unwrap();
    let (status, body) = send(
        &ctx.app,
        post_frame_req(&ctx, token_d, "k", "text/plain", b"x"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "invalid_token");

    // 缺 Idempotency-Key。
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/channels/{}/frames", ctx.channel))
        .header("authorization", auth(&ctx.token_a))
        .header("content-type", "text/plain")
        .body(Body::from("x"))
        .unwrap();
    let (status, body) = send(&ctx.app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "missing_idempotency_key");

    // 空 payload。
    let (status, body) = send(
        &ctx.app,
        post_frame_req(&ctx, &ctx.token_a, "empty", "text/plain", b""),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "empty_payload");

    // 非法 content type。
    let long_type = "a".repeat(129);
    let (status, body) = send(
        &ctx.app,
        post_frame_req(&ctx, &ctx.token_a, "long", &long_type, b"x"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_content_type");

    // payload 超过请求体上限。
    let too_large = vec![0u8; 2048];
    let (status, _) = send(
        &ctx.app,
        post_frame_req(
            &ctx,
            &ctx.token_a,
            "big",
            "application/octet-stream",
            &too_large,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    // 非法 cursor / limit。
    let (status, body) = send(&ctx.app, get_frames_req(&ctx, &ctx.token_a, "cursor=-1")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_cursor");

    let (status, body) = send(&ctx.app, get_frames_req(&ctx, &ctx.token_a, "limit=0")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_limit");
}

#[tokio::test]
async fn enrollment_and_membership_flow() {
    let ctx = setup().await;

    // 成员签发一次性入网码。
    let (status, body) = send(
        &ctx.app,
        post_json(
            format!("/v1/channels/{}/enroll-codes", ctx.channel),
            Some(&ctx.token_a),
            "{}".to_owned(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let code = body["enroll_code"].as_str().unwrap().to_owned();
    assert!(body["expires_at"].as_i64().unwrap() > 0);

    // 新 device 入网。
    let new_device = "1".repeat(32);
    let (status, body) = send(
        &ctx.app,
        post_json(
            "/v1/enroll".to_owned(),
            None,
            format!("{{\"enroll_code\":\"{code}\",\"device_id\":\"{new_device}\"}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let new_token = body["token"].as_str().unwrap().to_owned();
    assert_eq!(body["device_id"].as_str(), Some(new_device.as_str()));
    assert_eq!(body["channel_id"].as_str(), Some(ctx.channel.as_str()));

    let info = Request::builder()
        .uri(format!("/v1/channels/{}/info", ctx.channel))
        .header("authorization", auth(&new_token))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&ctx.app, info).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["device_id"].as_str(), Some(new_device.as_str()));

    // 同一个 code 只能用一次。
    let (status, body) = send(
        &ctx.app,
        post_json(
            "/v1/enroll".to_owned(),
            None,
            format!(
                "{{\"enroll_code\":\"{code}\",\"device_id\":\"{}\"}}",
                "2".repeat(32)
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "enroll_code_used");

    // device 已存在时返回 409，且不消耗 code。
    let (_, body) = send(
        &ctx.app,
        post_json(
            format!("/v1/channels/{}/enroll-codes", ctx.channel),
            Some(&ctx.token_a),
            "{}".to_owned(),
        ),
    )
    .await;
    let code2 = body["enroll_code"].as_str().unwrap().to_owned();
    let (status, body) = send(
        &ctx.app,
        post_json(
            "/v1/enroll".to_owned(),
            None,
            format!(
                "{{\"enroll_code\":\"{code2}\",\"device_id\":\"{}\"}}",
                ctx.device_a
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "device_exists");
    let (status, _) = send(
        &ctx.app,
        post_json(
            "/v1/enroll".to_owned(),
            None,
            format!(
                "{{\"enroll_code\":\"{code2}\",\"device_id\":\"{}\"}}",
                "3".repeat(32)
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 已有 device 用 membership 接口加入第二个 channel。
    let channel2 = "9".repeat(32);
    ctx.db
        .with_conn(|conn| {
            store::insert_channel(conn, &channel2, 4096, Some(3600), Some(1024), 1_000)
        })
        .unwrap();
    ctx.db
        .with_conn(|conn| store::upsert_membership(conn, &channel2, &ctx.device_a, 1_000))
        .unwrap();
    let (_, body) = send(
        &ctx.app,
        post_json(
            format!("/v1/channels/{channel2}/enroll-codes"),
            Some(&ctx.token_a),
            "{}".to_owned(),
        ),
    )
    .await;
    let code3 = body["enroll_code"].as_str().unwrap().to_owned();
    let (status, body) = send(
        &ctx.app,
        post_json(
            format!("/v1/channels/{channel2}/memberships"),
            Some(&ctx.token_b),
            format!("{{\"enroll_code\":\"{code3}\"}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["channel_id"].as_str(), Some(channel2.as_str()));
    assert_eq!(body["device_id"].as_str(), Some(ctx.device_b.as_str()));

    // 只能移除自己。
    let del = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/v1/channels/{channel2}/memberships/{}",
            ctx.device_b
        ))
        .header("authorization", auth(&ctx.token_a))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&ctx.app, del).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden");

    let del = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/v1/channels/{channel2}/memberships/{}",
            ctx.device_b
        ))
        .header("authorization", auth(&ctx.token_b))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&ctx.app, del).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["removed"], true);

    let info = Request::builder()
        .uri(format!("/v1/channels/{channel2}/info"))
        .header("authorization", auth(&ctx.token_b))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&ctx.app, info).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "not_a_member");
}

#[tokio::test]
async fn rate_limit_returns_429_with_retry_after() {
    let ctx = setup_with_rate(1).await;
    let (status, _) = send(&ctx.app, get_frames_req(&ctx, &ctx.token_a, "cursor=0")).await;
    assert_eq!(status, StatusCode::OK);

    let response = ctx
        .app
        .clone()
        .oneshot(get_frames_req(&ctx, &ctx.token_a, "cursor=0"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().get("retry-after").is_some());
}

#[tokio::test]
async fn two_device_push_pull_ack_gc_and_gap() {
    let ctx = setup().await;

    // A 推 3 帧。
    for index in 0..3 {
        let (status, _) = send(
            &ctx.app,
            post_frame_req(
                &ctx,
                &ctx.token_a,
                &format!("k{index}"),
                "text/plain",
                b"payload",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    // B 拉取并 ack 到 3。
    let (status, body) = send(
        &ctx.app,
        get_frames_req(&ctx, &ctx.token_b, "cursor=0&limit=10"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["frames"].as_array().unwrap().len(), 3);
    let ack = Request::builder()
        .method("POST")
        .uri(format!("/v1/channels/{}/ack", ctx.channel))
        .header("authorization", auth(&ctx.token_b))
        .header("content-type", "application/json")
        .body(Body::from("{\"cursor\":3}"))
        .unwrap();
    let (status, _) = send(&ctx.app, ack).await;
    assert_eq!(status, StatusCode::OK);

    // A 是发送者，B 已 ack 全部前缀；GC 删除 1..3。
    let report = ctx
        .db
        .with_conn_mut(|conn| store::gc_channel(conn, &ctx.channel, 3600, util::now_secs(), false))
        .unwrap();
    assert_eq!(report.deleted, 3);
    assert_eq!(report.retained_from, 4);

    // A 再写一帧；B 用旧 cursor 读取会看到 retained_from 缺口。
    let (status, _) = send(
        &ctx.app,
        post_frame_req(&ctx, &ctx.token_a, "k4", "text/plain", b"next"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(
        &ctx.app,
        get_frames_req(&ctx, &ctx.token_b, "cursor=0&limit=10"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["retained_from"].as_i64(), Some(4));
    let frames = body["frames"].as_array().unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["seq"].as_i64(), Some(4));
}

#[tokio::test]
async fn quota_full_returns_507() {
    let ctx = setup().await;
    let channel = "7".repeat(32);
    ctx.db
        .with_conn(|conn| store::insert_channel(conn, &channel, 4, Some(3600), Some(1024), 1_000))
        .unwrap();
    ctx.db
        .with_conn(|conn| store::upsert_membership(conn, &channel, &ctx.device_a, 1_000))
        .unwrap();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/channels/{channel}/frames"))
        .header("authorization", auth(&ctx.token_a))
        .header("idempotency-key", "q")
        .header("content-type", "text/plain")
        .body(Body::from("hello"))
        .unwrap();
    let (status, body) = send(&ctx.app, req).await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(body["error"], "storage_full");
}

#[tokio::test]
async fn enroll_rate_limit_is_enforced() {
    let ctx = setup().await;
    let device = "1".repeat(32);
    let body = format!("{{\"enroll_code\":\"bad\",\"device_id\":\"{device}\"}}");
    for _ in 0..10 {
        let req = Request::builder()
            .method("POST")
            .uri("/v1/enroll")
            .header("content-type", "application/json")
            .header("x-forwarded-for", "203.0.113.9")
            .body(Body::from(body.clone()))
            .unwrap();
        let (status, _) = send(&ctx.app, req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let req = Request::builder()
        .method("POST")
        .uri("/v1/enroll")
        .header("content-type", "application/json")
        .header("x-forwarded-for", "203.0.113.9")
        .body(Body::from(body))
        .unwrap();
    let (status, body) = send(&ctx.app, req).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"], "rate_limited");
}
