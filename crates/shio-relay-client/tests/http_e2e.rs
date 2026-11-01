//! 两设备 + 真实 HTTP relay 的端到端测试。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::time::Duration;

use shio_core::crypto::kdf::KdfParams;
use shio_core::model::RecordId;
use shio_core::store::{Engine, EngineOptions};
use shio_relay::db::Db;
use shio_relay::server::{router, AppState};
use shio_relay::{store, util, Config};
use shio_relay_client::HttpRelayTransport;
use shio_sync::relay::{channel_id, RelayClient, RelayError};
use shio_sync::{decode_pairing_secret, Sync, SyncInfo};

const PASSWORD: &[u8] = b"correct horse battery staple";

struct Ctx {
    _dir: tempfile::TempDir,
    addr: SocketAddr,
    channel: String,
    a: Engine,
    b: Engine,
    info_a: SyncInfo,
    info_b: SyncInfo,
    db: Db,
}

fn fast_kdf() -> KdfParams {
    KdfParams::new(8_192, 1, 1)
}

fn setup() -> Ctx {
    let dir = tempfile::tempdir().unwrap();
    let a = Engine::open(
        EngineOptions::new(dir.path().join("a.redb"))
            .create(true)
            .kdf(fast_kdf()),
        PASSWORD,
    )
    .unwrap();
    let info_a = a.enable_sync(None).unwrap();
    let sync_a = Sync::new(&a);
    let offer = sync_a.pairing_offer(Duration::from_secs(600)).unwrap();
    let secret = decode_pairing_secret(&offer.secret).unwrap();
    let b = Sync::accept_pairing(
        EngineOptions::new(dir.path().join("b.redb"))
            .create(true)
            .kdf(fast_kdf()),
        PASSWORD,
        &offer,
        &secret,
    )
    .unwrap();
    let info_b = b.sync_info().unwrap();

    let channel = channel_id(&info_a.db_id);
    let config = Config {
        data: dir.path().join("relay.db"),
        max_payload_bytes: 4 * 1024 * 1024,
        channel_quota_bytes: 8 * 1024 * 1024,
        ..Config::default()
    };
    let db = Db::open(&config.data).unwrap();
    db.with_conn(|conn| {
        store::insert_channel(
            conn,
            &channel,
            8 * 1024 * 1024,
            Some(3600),
            Some(4 * 1024 * 1024),
            1,
        )
    })
    .unwrap();
    let device_a = info_a.device_id.to_string();
    let device_b = info_b.device_id.to_string();
    db.with_conn(|conn| store::insert_device(conn, &device_a, &util::sha256_hex(b"token-a"), 1))
        .unwrap();
    db.with_conn(|conn| store::insert_device(conn, &device_b, &util::sha256_hex(b"token-b"), 1))
        .unwrap();
    db.with_conn(|conn| store::upsert_membership(conn, &channel, &device_a, 1))
        .unwrap();
    db.with_conn(|conn| store::upsert_membership(conn, &channel, &device_b, 1))
        .unwrap();

    let app = router(AppState::new(config, db.clone()));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    let addr = rx.recv().unwrap();
    Ctx {
        _dir: dir,
        addr,
        channel,
        a,
        b,
        info_a,
        info_b,
        db,
    }
}

#[test]
fn two_devices_over_http() {
    let ctx = setup();
    let url = format!("http://{}", ctx.addr);
    let mut client_a = RelayClient::new(
        Sync::new(&ctx.a),
        HttpRelayTransport::new(url.clone(), "token-a").unwrap(),
    );
    let mut client_b = RelayClient::new(
        Sync::new(&ctx.b),
        HttpRelayTransport::new(url.clone(), "token-b").unwrap(),
    );

    // A 推变更；重复推送复用幂等键。
    let id_a = RecordId::new_v7();
    ctx.a.put("notes", "note", id_a, &"from-a", 1).unwrap();
    let first = client_a.push_changes(0, 100).unwrap().unwrap();
    let retry = client_a.push_changes(0, 100).unwrap().unwrap();
    assert_eq!(first, retry);

    let report = client_b.pull_apply(0, 100).unwrap();
    assert_eq!(report.applied, 1);
    assert!(!report.gap);
    assert_eq!(
        ctx.b
            .get::<String>("notes", "note", &id_a)
            .unwrap()
            .unwrap()
            .value,
        "from-a"
    );

    // B 推变更；A 拉取。
    let id_b = RecordId::new_v7();
    ctx.b.put("notes", "note", id_b, &"from-b", 1).unwrap();
    client_b.push_changes(0, 100).unwrap().unwrap();
    client_a.pull_apply(0, 100).unwrap();
    assert_eq!(
        ctx.a
            .get::<String>("notes", "note", &id_b)
            .unwrap()
            .unwrap()
            .value,
        "from-b"
    );

    // 轮换与进度。
    ctx.a.rotate_data_key().unwrap();
    client_a.push_offers().unwrap().unwrap();
    let report = client_b.pull_apply(0, 100).unwrap();
    assert!(report.controls >= 1);
    assert_eq!(
        ctx.b.sync_info().unwrap().current_key_id,
        ctx.a.sync_info().unwrap().current_key_id
    );

    client_b.push_progress(ctx.info_a.device_id).unwrap();
    let report = client_a.pull_apply(0, 100).unwrap();
    assert!(report.controls >= 1);

    // 快照。
    let snapshots = client_a.push_snapshot(10).unwrap();
    assert!(!snapshots.is_empty());
    let report = client_b.pull_apply(0, 100).unwrap();
    assert!(report.applied >= 1);

    // info / ack。
    let info = client_b.info().unwrap();
    assert_eq!(info.device_id, ctx.info_b.device_id.to_string());
    let acked = client_b.ack(info.next_seq - 1).unwrap();
    assert_eq!(acked, info.next_seq - 1);

    // 错误 token 的 401 映射。
    let mut bad = RelayClient::new(
        Sync::new(&ctx.b),
        HttpRelayTransport::new(url.clone(), "bad-token").unwrap(),
    );
    match bad.pull_apply(0, 100) {
        Err(RelayError::Http { status, code, .. }) => {
            assert_eq!(status, 401);
            assert_eq!(code, "invalid_token");
        }
        other => panic!("unexpected result: {other:?}"),
    }

    // TTL 淘汰后旧 cursor 触发 gap。
    let now = util::now_secs();
    ctx.db
        .with_conn_mut(|conn| store::gc_channel(conn, &ctx.channel, 3600, now + 4000, false))
        .unwrap();
    let report = client_b.pull_apply(0, 100).unwrap();
    assert!(report.gap);
}
