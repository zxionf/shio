//! Relay 客户端适配的内存端到端测试。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shio_core::crypto::kdf::KdfParams;
use shio_core::model::RecordId;
use shio_core::store::{Engine, EngineOptions};
use shio_sync::relay::{
    RelayClient, RelayFrame, RelayInfo, RelayPage, RelayTransport, SHIO_CHANGES, SHIO_OFFERS,
    SHIO_PROGRESS, SHIO_SNAPSHOT,
};
use shio_sync::{decode_pairing_secret, DeviceId, Result, Sync};

const PASSWORD: &[u8] = b"correct horse battery staple";

fn fast_kdf() -> KdfParams {
    KdfParams::new(8_192, 1, 1)
}

fn options(path: &Path) -> EngineOptions {
    EngineOptions::new(path).kdf(fast_kdf())
}

fn create(path: &Path) -> Engine {
    Engine::open(options(path).create(true), PASSWORD).unwrap()
}

struct State {
    frames: Vec<RelayFrame>,
    next_seq: i64,
    retained_from: i64,
    acks: HashMap<DeviceId, i64>,
    idem: HashMap<(DeviceId, String), i64>,
}

#[derive(Clone)]
struct MemRelay {
    state: Arc<Mutex<State>>,
    sender: DeviceId,
}

impl MemRelay {
    fn pair(a: DeviceId, b: DeviceId) -> (Self, Self) {
        let state = Arc::new(Mutex::new(State {
            frames: Vec::new(),
            next_seq: 1,
            retained_from: 1,
            acks: HashMap::new(),
            idem: HashMap::new(),
        }));
        (
            Self {
                state: state.clone(),
                sender: a,
            },
            Self { state, sender: b },
        )
    }

    fn frames(&self) -> Vec<RelayFrame> {
        self.state.lock().unwrap().frames.clone()
    }

    fn retain_from(&self, seq: i64) {
        self.state.lock().unwrap().retained_from = seq;
    }
}

impl RelayTransport for MemRelay {
    fn push(
        &mut self,
        _channel_id: &str,
        content_type: &str,
        payload: &[u8],
        idempotency_key: &str,
    ) -> Result<i64> {
        let mut state = self.state.lock().unwrap();
        let key = (self.sender, idempotency_key.to_owned());
        if let Some(seq) = state.idem.get(&key) {
            return Ok(*seq);
        }
        let seq = state.next_seq;
        state.next_seq += 1;
        state.frames.push(RelayFrame {
            seq,
            sender: self.sender,
            content_type: content_type.to_owned(),
            payload: payload.to_vec(),
        });
        state.idem.insert(key, seq);
        Ok(seq)
    }

    fn pull(&mut self, _channel_id: &str, cursor: i64, limit: usize) -> Result<RelayPage> {
        let state = self.state.lock().unwrap();
        let mut frames = Vec::new();
        let mut has_more = false;
        for frame in state.frames.iter().filter(|frame| {
            frame.seq > cursor && frame.seq >= state.retained_from && frame.sender != self.sender
        }) {
            if frames.len() >= limit {
                has_more = true;
                break;
            }
            frames.push(frame.clone());
        }
        let next_cursor = frames.last().map(|frame| frame.seq).unwrap_or(cursor);
        Ok(RelayPage {
            frames,
            next_cursor,
            has_more,
            retained_from: state.retained_from,
        })
    }

    fn ack(&mut self, _channel_id: &str, cursor: i64) -> Result<i64> {
        let mut state = self.state.lock().unwrap();
        let entry = state.acks.entry(self.sender).or_insert(0);
        *entry = (*entry).max(cursor);
        Ok(*entry)
    }

    fn info(&mut self, channel_id: &str) -> Result<RelayInfo> {
        let state = self.state.lock().unwrap();
        let acked_cursor = *state.acks.get(&self.sender).unwrap_or(&0);
        let pending: Vec<&RelayFrame> = state
            .frames
            .iter()
            .filter(|frame| frame.seq > acked_cursor && frame.sender != self.sender)
            .collect();
        Ok(RelayInfo {
            channel_id: channel_id.to_owned(),
            device_id: self.sender.to_string(),
            acked_cursor,
            next_seq: state.next_seq,
            pending_frames: pending.len() as i64,
            pending_bytes: pending.iter().map(|frame| frame.payload.len() as i64).sum(),
            quota_bytes: 0,
            frame_ttl_secs: 0,
            retained_from: state.retained_from,
            max_payload_bytes: 0,
        })
    }
}

#[test]
fn relay_client_pushes_pulls_and_applies() {
    let dir = tempfile::tempdir().unwrap();
    let a = create(&dir.path().join("a.redb"));
    let info_a = a.enable_sync(None).unwrap();
    let sync_a = Sync::new(&a);
    let offer = sync_a.pairing_offer(Duration::from_secs(600)).unwrap();
    let secret = decode_pairing_secret(&offer.secret).unwrap();
    let b = Sync::accept_pairing(
        options(&dir.path().join("b.redb")),
        PASSWORD,
        &offer,
        &secret,
    )
    .unwrap();
    let info_b = b.sync_info().unwrap();

    let (relay_a, relay_b) = MemRelay::pair(info_a.device_id, info_b.device_id);
    let mut client_a = RelayClient::new(Sync::new(&a), relay_a);
    let mut client_b = RelayClient::new(Sync::new(&b), relay_b);

    // A 写一条并推送；幂等键让重复推送返回同一 seq。
    let id_a = RecordId::new_v7();
    a.put("notes", "note", id_a, &"from-a", 1).unwrap();
    let first = client_a.push_changes(0, 100).unwrap().unwrap();
    assert_eq!(first, 1);
    let retry = client_a.push_changes(0, 100).unwrap().unwrap();
    assert_eq!(retry, first);
    assert_eq!(client_a.transport().frames().len(), 1);
    assert_eq!(client_a.transport().frames()[0].content_type, SHIO_CHANGES);

    let report = client_b.pull_apply(0, 100).unwrap();
    assert_eq!(report.applied, 1);
    assert!(!report.gap);
    assert_eq!(
        b.get::<String>("notes", "note", &id_a)
            .unwrap()
            .unwrap()
            .value,
        "from-a"
    );

    // B 写一条并推送；A 拉取。
    let id_b = RecordId::new_v7();
    b.put("notes", "note", id_b, &"from-b", 1).unwrap();
    client_b.push_changes(0, 100).unwrap().unwrap();
    client_a.pull_apply(0, 100).unwrap();
    assert_eq!(
        a.get::<String>("notes", "note", &id_b)
            .unwrap()
            .unwrap()
            .value,
        "from-b"
    );

    // 轮换：A 推 offer，B 应用后拿到新 key。
    a.rotate_data_key().unwrap();
    let offer_seq = client_a.push_offers().unwrap().unwrap();
    assert!(offer_seq > 0);
    let report = client_b.pull_apply(0, 100).unwrap();
    assert!(report.controls >= 1);
    assert_eq!(
        b.sync_info().unwrap().current_key_id,
        a.sync_info().unwrap().current_key_id
    );
    assert!(client_a
        .transport()
        .frames()
        .iter()
        .any(|frame| frame.content_type == SHIO_OFFERS));

    // 进度帧也走同一通道。
    let progress_seq = client_b.push_progress(info_a.device_id).unwrap();
    assert!(progress_seq > 0);
    let report = client_a.pull_apply(0, 100).unwrap();
    assert!(report.controls >= 1);
    assert!(client_b
        .transport()
        .frames()
        .iter()
        .any(|frame| frame.content_type == SHIO_PROGRESS));

    // 快照页可被另一端拉取应用。
    let snapshot_seqs = client_a.push_snapshot(10).unwrap();
    assert!(!snapshot_seqs.is_empty());
    assert!(client_a
        .transport()
        .frames()
        .iter()
        .any(|frame| frame.content_type == SHIO_SNAPSHOT));
    let report = client_b.pull_apply(0, 100).unwrap();
    assert!(report.applied >= 1);

    // info/ack 往返。
    let info = client_b.info().unwrap();
    let acked = client_b.ack(info.next_seq - 1).unwrap();
    assert_eq!(acked, info.next_seq - 1);
    assert_eq!(client_b.info().unwrap().acked_cursor, acked);

    // 缺口标记：cursor 低于 retained_from 时报告 gap；没有可读帧时 applied=0。
    client_a.transport().retain_from(999);
    let report = client_b.pull_apply(0, 100).unwrap();
    assert!(report.gap);
    assert_eq!(report.applied, 0);
}

#[test]
fn gap_applies_snapshot_and_retained_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let a = create(&dir.path().join("a.redb"));
    let info_a = a.enable_sync(None).unwrap();
    let sync_a = Sync::new(&a);
    let offer = sync_a.pairing_offer(Duration::from_secs(600)).unwrap();
    let secret = decode_pairing_secret(&offer.secret).unwrap();
    let b = Sync::accept_pairing(
        options(&dir.path().join("b.redb")),
        PASSWORD,
        &offer,
        &secret,
    )
    .unwrap();
    let info_b = b.sync_info().unwrap();

    let (relay_a, relay_b) = MemRelay::pair(info_a.device_id, info_b.device_id);
    let mut client_a = RelayClient::new(Sync::new(&a), relay_a);
    let mut client_b = RelayClient::new(Sync::new(&b), relay_b);

    let id = RecordId::new_v7();
    a.put("notes", "note", id, &"snapshot", 1).unwrap();
    client_a.push_changes(0, 100).unwrap().unwrap();
    let snapshot_seqs = client_a.push_snapshot(10).unwrap();
    assert!(!snapshot_seqs.is_empty());
    let first_snapshot = *snapshot_seqs.first().unwrap();
    assert!(first_snapshot > 1);

    // 模拟 relay 已淘汰快照之前的帧；B 的旧 cursor 触发 gap。
    client_a.transport().retain_from(first_snapshot);
    let report = client_b.pull_apply(0, 100).unwrap();
    assert!(report.gap);
    assert!(report.applied >= 1);
    assert_eq!(report.retained_from, first_snapshot);
    assert_eq!(
        b.get::<String>("notes", "note", &id)
            .unwrap()
            .unwrap()
            .value,
        "snapshot"
    );

    // 从 retained_from 重拉不再是 gap。
    let report = client_b.pull_from_retained(100).unwrap();
    assert!(!report.gap);
    assert!(report.next_cursor >= first_snapshot);
}
