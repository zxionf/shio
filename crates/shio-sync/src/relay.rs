//! Relay 客户端适配：把 `Sync` 的帧搬运到 `RelayTransport`。
//!
//! 本模块不依赖网络；HTTP 客户端实现 [`RelayTransport`] 即可。它负责
//! `db_id` ↔ `channel_id` 映射、`content_type` 分派、幂等键与 gap 标记。

use sha2::{Digest, Sha256};

use crate::{DeviceId, Result, Sync};

/// 变更帧的 content type hint。
pub const SHIO_CHANGES: &str = "application/vnd.shio.shy2.changes";
/// offer 帧的 content type hint。
pub const SHIO_OFFERS: &str = "application/vnd.shio.shy2.offers";
/// 快照帧的 content type hint。
pub const SHIO_SNAPSHOT: &str = "application/vnd.shio.shy2.snapshot";
/// 进度帧的 content type hint。
pub const SHIO_PROGRESS: &str = "application/vnd.shio.shy2.progress";

/// relay 返回的一帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayFrame {
    /// relay 分配的序号。
    pub seq: i64,
    /// 上传设备。
    pub sender: DeviceId,
    /// content type hint。
    pub content_type: String,
    /// 原始 payload。
    pub payload: Vec<u8>,
}

/// relay 拉取结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayPage {
    /// 帧列表。
    pub frames: Vec<RelayFrame>,
    /// 下一页 cursor。
    pub next_cursor: i64,
    /// 是否还有更多。
    pub has_more: bool,
    /// 最小仍可读的 seq。
    pub retained_from: i64,
}

/// relay `info` 的子集。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayInfo {
    /// channel ID。
    pub channel_id: String,
    /// device ID（hex）。
    pub device_id: String,
    /// 已 ack 的 seq。
    pub acked_cursor: i64,
    /// 下一个 seq。
    pub next_seq: i64,
    /// 待拉取帧数。
    pub pending_frames: i64,
    /// 待拉取字节数。
    pub pending_bytes: i64,
    /// quota。
    pub quota_bytes: i64,
    /// 帧 TTL。
    pub frame_ttl_secs: i64,
    /// 最小可读 seq。
    pub retained_from: i64,
    /// 单帧上限。
    pub max_payload_bytes: i64,
}

/// relay 传输接口；HTTP 客户端只需实现这四个方法。
pub trait RelayTransport {
    /// 上传一帧；返回 relay 分配的 `seq`。
    fn push(
        &mut self,
        channel_id: &str,
        content_type: &str,
        payload: &[u8],
        idempotency_key: &str,
    ) -> Result<i64>;
    /// 拉取 `seq > cursor` 的帧；默认由实现排除自己。
    fn pull(&mut self, channel_id: &str, cursor: i64, limit: usize) -> Result<RelayPage>;
    /// 推进 ack；返回落盘后的 cursor。
    fn ack(&mut self, channel_id: &str, cursor: i64) -> Result<i64>;
    /// 读取 channel 摘要。
    fn info(&mut self, channel_id: &str) -> Result<RelayInfo>;
}

/// 一次 pull + apply 的结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PullReport {
    /// 本次拉到的帧数。
    pub frames: usize,
    /// 实际写入的记录数。
    pub applied: usize,
    /// 因 LWW/重复/未知 content type 跳过的数量。
    pub skipped: usize,
    /// offers/progress 等控制帧数量。
    pub controls: usize,
    /// 下一页 cursor。
    pub next_cursor: i64,
    /// 是否还有更多。
    pub has_more: bool,
    /// 最小可读 seq。
    pub retained_from: i64,
    /// `retained_from > cursor + 1`，需要快照重同步。
    pub gap: bool,
}

/// 把 `Sync` 与 `RelayTransport` 组合起来的客户端。
pub struct RelayClient<'a, T> {
    sync: Sync<'a>,
    transport: T,
}

impl<'a, T: RelayTransport> RelayClient<'a, T> {
    /// 创建客户端。
    pub fn new(sync: Sync<'a>, transport: T) -> Self {
        Self { sync, transport }
    }

    /// 底层 `Sync`。
    pub fn sync(&self) -> &Sync<'a> {
        &self.sync
    }

    /// 底层 transport。
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// 底层 transport 的可变引用。
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// `channel_id = hex(db_id)`。
    pub fn channel_id(&self) -> Result<String> {
        let info = self.sync.info()?;
        Ok(hex(&info.db_id))
    }

    /// 推送一页变更；空页返回 `None`。
    pub fn push_changes(&mut self, after: u64, limit: usize) -> Result<Option<i64>> {
        let page = self.sync.changes_since(after, limit)?;
        if page.entries.is_empty() {
            return Ok(None);
        }
        let frame = self.sync.encode(&page)?;
        self.push_frame(SHIO_CHANGES, &frame).map(Some)
    }

    /// 推送 offer 页；没有 offer 返回 `None`。
    pub fn push_offers(&mut self) -> Result<Option<i64>> {
        let page = self.sync.offers()?;
        if page.offers.is_empty() {
            return Ok(None);
        }
        let frame = self.sync.encode_offers(&page)?;
        self.push_frame(SHIO_OFFERS, &frame).map(Some)
    }

    /// 推送进度帧。
    pub fn push_progress(&mut self, peer: DeviceId) -> Result<i64> {
        let frame = self.sync.progress_frame(peer)?;
        self.push_frame(SHIO_PROGRESS, &frame)
    }

    /// 推送本机快照的所有页；返回各页的 relay seq。
    pub fn push_snapshot(&mut self, limit: usize) -> Result<Vec<i64>> {
        let mut snapshot = self.sync.snapshot()?;
        let mut seqs = Vec::new();
        while let Some(page) = snapshot.next_page(limit)? {
            let frame = self.sync.encode_snapshot(&page)?;
            seqs.push(self.push_frame(SHIO_SNAPSHOT, &frame)?);
        }
        Ok(seqs)
    }

    /// 拉取并应用一页；按 `content_type` 分派到 checked apply。
    ///
    /// `gap = true` 时不应用任何帧，调用方应走快照重同步。
    pub fn pull_apply(&mut self, cursor: i64, limit: usize) -> Result<PullReport> {
        let channel_id = self.channel_id()?;
        let page = self.transport.pull(&channel_id, cursor, limit)?;
        let mut report = PullReport {
            frames: page.frames.len(),
            next_cursor: page.next_cursor,
            has_more: page.has_more,
            retained_from: page.retained_from,
            ..PullReport::default()
        };
        if page.retained_from > cursor.saturating_add(1) {
            report.gap = true;
            return Ok(report);
        }
        for frame in page.frames {
            match frame.content_type.as_str() {
                SHIO_CHANGES => {
                    let applied = self.sync.apply_checked(&frame.payload, frame.sender)?;
                    report.applied += applied.applied;
                    report.skipped += applied.skipped;
                }
                SHIO_SNAPSHOT => {
                    let applied = self
                        .sync
                        .apply_snapshot_frame_checked(&frame.payload, frame.sender)?;
                    report.applied += applied.applied;
                    report.skipped += applied.skipped;
                }
                SHIO_OFFERS => {
                    self.sync
                        .apply_offers_checked(&frame.payload, frame.sender)?;
                    report.controls += 1;
                }
                SHIO_PROGRESS => {
                    self.sync
                        .apply_progress_checked(&frame.payload, frame.sender)?;
                    report.controls += 1;
                }
                _ => report.skipped += 1,
            }
        }
        Ok(report)
    }

    /// 推进 relay ack。
    pub fn ack(&mut self, cursor: i64) -> Result<i64> {
        let channel_id = self.channel_id()?;
        self.transport.ack(&channel_id, cursor)
    }

    /// 读取 relay info。
    pub fn info(&mut self) -> Result<RelayInfo> {
        let channel_id = self.channel_id()?;
        self.transport.info(&channel_id)
    }

    fn push_frame(&mut self, content_type: &str, frame: &[u8]) -> Result<i64> {
        let channel_id = self.channel_id()?;
        let idem_key = idempotency_key(frame);
        self.transport
            .push(&channel_id, content_type, frame, &idem_key)
    }
}

/// 帧的幂等键：SHA-256 hex；重试同一帧会复用同一 key。
pub fn idempotency_key(frame: &[u8]) -> String {
    let digest = Sha256::digest(frame);
    hex(digest.as_slice())
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}
