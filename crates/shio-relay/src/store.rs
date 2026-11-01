//! channel / frame / device / membership / enroll code 数据访问。

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::error::{RelayError, Result};

/// channel 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    /// channel ID。
    pub channel_id: String,
    /// 下一个分配的 seq。
    pub next_seq: i64,
    /// 最小仍可读的 seq。
    pub retained_from: i64,
    /// 当前 payload 字节数。
    pub bytes: i64,
    /// 配额。
    pub quota_bytes: i64,
    /// 帧 TTL；`None` 表示用全局默认。
    pub frame_ttl_secs: Option<i64>,
    /// 单帧上限；`None` 表示用全局默认。
    pub max_payload_bytes: Option<i64>,
    /// 创建时间。
    pub created_at: i64,
}

impl Channel {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            channel_id: row.get(0)?,
            next_seq: row.get(1)?,
            retained_from: row.get(2)?,
            bytes: row.get(3)?,
            quota_bytes: row.get(4)?,
            frame_ttl_secs: row.get(5)?,
            max_payload_bytes: row.get(6)?,
            created_at: row.get(7)?,
        })
    }
}

/// device 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// device ID。
    pub device_id: String,
    /// token 的 SHA-256 hex。
    pub token_hash: String,
    /// 创建时间。
    pub created_at: i64,
    /// 最近访问时间。
    pub last_seen: Option<i64>,
    /// 全局撤销时间。
    pub revoked_at: Option<i64>,
}

impl Device {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            device_id: row.get(0)?,
            token_hash: row.get(1)?,
            created_at: row.get(2)?,
            last_seen: row.get(3)?,
            revoked_at: row.get(4)?,
        })
    }
}

/// membership 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Membership {
    /// channel ID。
    pub channel_id: String,
    /// device ID。
    pub device_id: String,
    /// 已 ack 的 seq。
    pub acked_cursor: i64,
    /// 加入时间。
    pub created_at: i64,
    /// 移除时间。
    pub revoked_at: Option<i64>,
}

impl Membership {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            channel_id: row.get(0)?,
            device_id: row.get(1)?,
            acked_cursor: row.get(2)?,
            created_at: row.get(3)?,
            revoked_at: row.get(4)?,
        })
    }
}

/// 入网码行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollCode {
    /// code 的 SHA-256 hex。
    pub code_hash: String,
    /// 目标 channel。
    pub channel_id: String,
    /// 过期时间。
    pub expires_at: i64,
    /// 使用时间。
    pub used_at: Option<i64>,
    /// 创建时间。
    pub created_at: i64,
}

impl EnrollCode {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            code_hash: row.get(0)?,
            channel_id: row.get(1)?,
            expires_at: row.get(2)?,
            used_at: row.get(3)?,
            created_at: row.get(4)?,
        })
    }
}

/// frame 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// channel ID。
    pub channel_id: String,
    /// 传输序号。
    pub seq: i64,
    /// 上传设备。
    pub sender: String,
    /// content type hint。
    pub content_type: String,
    /// 原始 payload。
    pub payload: Vec<u8>,
    /// 创建时间。
    pub created_at: i64,
}

impl Frame {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            channel_id: row.get(0)?,
            seq: row.get(1)?,
            sender: row.get(2)?,
            content_type: row.get(3)?,
            payload: row.get(4)?,
            created_at: row.get(5)?,
        })
    }
}

/// 追加结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendOutcome {
    /// 分配或复用的 seq。
    pub seq: i64,
    /// 是否命中幂等键。
    pub duplicate: bool,
}

/// 拉取结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramePage {
    /// 帧列表。
    pub frames: Vec<Frame>,
    /// 下一页 cursor。
    pub next_cursor: i64,
    /// 是否还有更多。
    pub has_more: bool,
    /// 最小仍可读的 seq。
    pub retained_from: i64,
}

/// 插入 channel。
pub fn insert_channel(
    conn: &Connection,
    channel_id: &str,
    quota_bytes: i64,
    frame_ttl_secs: Option<i64>,
    max_payload_bytes: Option<i64>,
    now: i64,
) -> Result<Channel> {
    let result = conn.execute(
        "INSERT INTO channels
         (channel_id, next_seq, retained_from, bytes, quota_bytes, frame_ttl_secs, max_payload_bytes, created_at)
         VALUES (?1, 1, 1, 0, ?2, ?3, ?4, ?5)",
        params![channel_id, quota_bytes, frame_ttl_secs, max_payload_bytes, now],
    );
    if let Err(err) = result {
        if is_constraint(&err) {
            return Err(RelayError::Conflict(format!(
                "channel {channel_id} already exists"
            )));
        }
        return Err(err.into());
    }
    get_channel(conn, channel_id)?.ok_or(RelayError::NotFound)
}

/// 读取 channel。
pub fn get_channel(conn: &Connection, channel_id: &str) -> Result<Option<Channel>> {
    let channel = conn
        .query_row(
            "SELECT channel_id, next_seq, retained_from, bytes, quota_bytes, frame_ttl_secs, max_payload_bytes, created_at
             FROM channels WHERE channel_id = ?1",
            params![channel_id],
            Channel::from_row,
        )
        .optional()?;
    Ok(channel)
}

/// 列出 channel。
pub fn list_channels(conn: &Connection) -> Result<Vec<Channel>> {
    let mut stmt = conn.prepare(
        "SELECT channel_id, next_seq, retained_from, bytes, quota_bytes, frame_ttl_secs, max_payload_bytes, created_at
         FROM channels ORDER BY channel_id",
    )?;
    let rows = stmt.query_map([], Channel::from_row)?;
    let channels = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(channels)
}

/// 插入 device。
pub fn insert_device(
    conn: &Connection,
    device_id: &str,
    token_hash: &str,
    now: i64,
) -> Result<Device> {
    let result = conn.execute(
        "INSERT INTO devices (device_id, token_hash, created_at, last_seen, revoked_at)
         VALUES (?1, ?2, ?3, NULL, NULL)",
        params![device_id, token_hash, now],
    );
    if let Err(err) = result {
        if is_constraint(&err) {
            return Err(RelayError::Conflict(format!(
                "device {device_id} or token already exists"
            )));
        }
        return Err(err.into());
    }
    device_by_id(conn, device_id)?.ok_or(RelayError::NotFound)
}

/// 按 token hash 查 device。
pub fn device_by_token_hash(conn: &Connection, token_hash: &str) -> Result<Option<Device>> {
    let device = conn
        .query_row(
            "SELECT device_id, token_hash, created_at, last_seen, revoked_at
             FROM devices WHERE token_hash = ?1",
            params![token_hash],
            Device::from_row,
        )
        .optional()?;
    Ok(device)
}

/// 按 device ID 查 device。
pub fn device_by_id(conn: &Connection, device_id: &str) -> Result<Option<Device>> {
    let device = conn
        .query_row(
            "SELECT device_id, token_hash, created_at, last_seen, revoked_at
             FROM devices WHERE device_id = ?1",
            params![device_id],
            Device::from_row,
        )
        .optional()?;
    Ok(device)
}

/// 全局撤销 device。
pub fn revoke_device(conn: &Connection, device_id: &str, now: i64) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE devices SET revoked_at = ?1 WHERE device_id = ?2 AND revoked_at IS NULL",
        params![now, device_id],
    )?;
    Ok(changed > 0)
}

/// 更新最近访问时间。
pub fn touch_device(conn: &Connection, device_id: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE devices SET last_seen = ?1 WHERE device_id = ?2",
        params![now, device_id],
    )?;
    Ok(())
}

/// 轮换 device token；旧 token 立即失效。
pub fn rotate_token(conn: &Connection, device_id: &str, token_hash: &str) -> Result<bool> {
    let result = conn.execute(
        "UPDATE devices SET token_hash = ?1 WHERE device_id = ?2 AND revoked_at IS NULL",
        params![token_hash, device_id],
    );
    match result {
        Ok(changed) => Ok(changed > 0),
        Err(err) if is_constraint(&err) => {
            Err(RelayError::Conflict("token already exists".to_owned()))
        }
        Err(err) => Err(err.into()),
    }
}

/// 加入或恢复 membership。
pub fn upsert_membership(
    conn: &Connection,
    channel_id: &str,
    device_id: &str,
    now: i64,
) -> Result<Membership> {
    if get_channel(conn, channel_id)?.is_none() {
        return Err(RelayError::NotFound);
    }
    let device = device_by_id(conn, device_id)?.ok_or(RelayError::NotFound)?;
    if device.revoked_at.is_some() {
        return Err(RelayError::Conflict("device is revoked".to_owned()));
    }
    conn.execute(
        "INSERT INTO memberships (channel_id, device_id, acked_cursor, created_at, revoked_at)
         VALUES (?1, ?2, 0, ?3, NULL)
         ON CONFLICT(channel_id, device_id) DO UPDATE SET revoked_at = NULL",
        params![channel_id, device_id, now],
    )?;
    membership(conn, channel_id, device_id)?.ok_or(RelayError::NotFound)
}

/// 读取 membership（包含已移除）。
pub fn membership(
    conn: &Connection,
    channel_id: &str,
    device_id: &str,
) -> Result<Option<Membership>> {
    let membership = conn
        .query_row(
            "SELECT channel_id, device_id, acked_cursor, created_at, revoked_at
             FROM memberships WHERE channel_id = ?1 AND device_id = ?2",
            params![channel_id, device_id],
            Membership::from_row,
        )
        .optional()?;
    Ok(membership)
}

/// 读取活跃 membership。
pub fn active_membership(
    conn: &Connection,
    channel_id: &str,
    device_id: &str,
) -> Result<Option<Membership>> {
    let membership = conn
        .query_row(
            "SELECT m.channel_id, m.device_id, m.acked_cursor, m.created_at, m.revoked_at
             FROM memberships m
             JOIN devices d ON d.device_id = m.device_id
             WHERE m.channel_id = ?1 AND m.device_id = ?2
               AND m.revoked_at IS NULL AND d.revoked_at IS NULL",
            params![channel_id, device_id],
            Membership::from_row,
        )
        .optional()?;
    Ok(membership)
}

/// 列出 channel 的活跃成员。
pub fn active_members(conn: &Connection, channel_id: &str) -> Result<Vec<Membership>> {
    let mut stmt = conn.prepare(
        "SELECT m.channel_id, m.device_id, m.acked_cursor, m.created_at, m.revoked_at
         FROM memberships m
         JOIN devices d ON d.device_id = m.device_id
         WHERE m.channel_id = ?1 AND m.revoked_at IS NULL AND d.revoked_at IS NULL
         ORDER BY m.device_id",
    )?;
    let rows = stmt.query_map(params![channel_id], Membership::from_row)?;
    let members = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(members)
}

/// 推进 ack；只前进不回退。
pub fn ack_membership(
    conn: &Connection,
    channel_id: &str,
    device_id: &str,
    cursor: i64,
) -> Result<i64> {
    let changed = conn.execute(
        "UPDATE memberships SET acked_cursor = MAX(acked_cursor, ?3)
         WHERE channel_id = ?1 AND device_id = ?2 AND revoked_at IS NULL
           AND EXISTS (
             SELECT 1 FROM devices WHERE device_id = ?2 AND revoked_at IS NULL
           )",
        params![channel_id, device_id, cursor],
    )?;
    if changed == 0 {
        return Err(RelayError::NotFound);
    }
    let acked = conn.query_row(
        "SELECT acked_cursor FROM memberships WHERE channel_id = ?1 AND device_id = ?2",
        params![channel_id, device_id],
        |row| row.get(0),
    )?;
    Ok(acked)
}

/// 移除 membership。
pub fn revoke_membership(
    conn: &Connection,
    channel_id: &str,
    device_id: &str,
    now: i64,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE memberships SET revoked_at = ?1
         WHERE channel_id = ?2 AND device_id = ?3 AND revoked_at IS NULL",
        params![now, channel_id, device_id],
    )?;
    Ok(changed > 0)
}

/// 插入入网码。
pub fn insert_enroll_code(
    conn: &Connection,
    code_hash: &str,
    channel_id: &str,
    expires_at: i64,
    now: i64,
) -> Result<()> {
    if get_channel(conn, channel_id)?.is_none() {
        return Err(RelayError::NotFound);
    }
    let result = conn.execute(
        "INSERT INTO enroll_codes (code_hash, channel_id, expires_at, used_at, created_at)
         VALUES (?1, ?2, ?3, NULL, ?4)",
        params![code_hash, channel_id, expires_at, now],
    );
    if let Err(err) = result {
        if is_constraint(&err) {
            return Err(RelayError::Conflict(
                "enroll code already exists".to_owned(),
            ));
        }
        return Err(err.into());
    }
    Ok(())
}

/// 消耗入网码。
pub fn consume_enroll_code(conn: &Connection, code_hash: &str, now: i64) -> Result<EnrollCode> {
    let code = enroll_code(conn, code_hash)?.ok_or(RelayError::NotFound)?;
    if code.used_at.is_some() {
        return Err(RelayError::Conflict("enroll code already used".to_owned()));
    }
    if code.expires_at <= now {
        return Err(RelayError::Expired("enroll code expired"));
    }
    if get_channel(conn, &code.channel_id)?.is_none() {
        return Err(RelayError::NotFound);
    }
    let changed = conn.execute(
        "UPDATE enroll_codes SET used_at = ?1 WHERE code_hash = ?2 AND used_at IS NULL",
        params![now, code_hash],
    )?;
    if changed == 0 {
        return Err(RelayError::Conflict("enroll code already used".to_owned()));
    }
    Ok(EnrollCode {
        used_at: Some(now),
        ..code
    })
}

/// 清理过期或已用的入网码。
pub fn delete_expired_enroll_codes(conn: &Connection, now: i64) -> Result<usize> {
    let changed = conn.execute(
        "DELETE FROM enroll_codes WHERE expires_at <= ?1 OR used_at IS NOT NULL",
        params![now],
    )?;
    Ok(changed)
}

/// 用入网码创建新 device 与 membership。
pub fn enroll_device(
    conn: &mut Connection,
    code_hash: &str,
    device_id: &str,
    token_hash: &str,
    now: i64,
) -> Result<(String, Device)> {
    let tx = conn.transaction()?;
    let code = enroll_code(&tx, code_hash)?.ok_or(RelayError::NotFound)?;
    if code.used_at.is_some() {
        return Err(RelayError::Conflict("enroll code already used".to_owned()));
    }
    if code.expires_at <= now {
        return Err(RelayError::Expired("enroll code expired"));
    }
    if get_channel(&tx, &code.channel_id)?.is_none() {
        return Err(RelayError::NotFound);
    }
    if device_by_id(&tx, device_id)?.is_some() {
        return Err(RelayError::Conflict("device_exists".to_owned()));
    }
    tx.execute(
        "INSERT INTO devices (device_id, token_hash, created_at, last_seen, revoked_at)
         VALUES (?1, ?2, ?3, NULL, NULL)",
        params![device_id, token_hash, now],
    )?;
    tx.execute(
        "INSERT INTO memberships (channel_id, device_id, acked_cursor, created_at, revoked_at)
         VALUES (?1, ?2, 0, ?3, NULL)",
        params![code.channel_id, device_id, now],
    )?;
    let changed = tx.execute(
        "UPDATE enroll_codes SET used_at = ?1 WHERE code_hash = ?2 AND used_at IS NULL",
        params![now, code_hash],
    )?;
    if changed == 0 {
        return Err(RelayError::Conflict("enroll code already used".to_owned()));
    }
    let device = device_by_id(&tx, device_id)?.ok_or(RelayError::NotFound)?;
    tx.commit()?;
    Ok((code.channel_id, device))
}

/// 已认证 device 用入网码加入 channel。
pub fn consume_code_for_membership(
    conn: &mut Connection,
    code_hash: &str,
    channel_id: &str,
    device_id: &str,
    now: i64,
) -> Result<Membership> {
    let tx = conn.transaction()?;
    let code = enroll_code(&tx, code_hash)?.ok_or(RelayError::NotFound)?;
    if code.channel_id != channel_id {
        return Err(RelayError::InvalidArgument(
            "enroll code belongs to another channel",
        ));
    }
    if code.used_at.is_some() {
        return Err(RelayError::Conflict("enroll code already used".to_owned()));
    }
    if code.expires_at <= now {
        return Err(RelayError::Expired("enroll code expired"));
    }
    let device = device_by_id(&tx, device_id)?.ok_or(RelayError::NotFound)?;
    if device.revoked_at.is_some() {
        return Err(RelayError::Conflict("device is revoked".to_owned()));
    }
    upsert_membership(&tx, channel_id, device_id, now)?;
    let changed = tx.execute(
        "UPDATE enroll_codes SET used_at = ?1 WHERE code_hash = ?2 AND used_at IS NULL",
        params![now, code_hash],
    )?;
    if changed == 0 {
        return Err(RelayError::Conflict("enroll code already used".to_owned()));
    }
    let membership = membership(&tx, channel_id, device_id)?.ok_or(RelayError::NotFound)?;
    tx.commit()?;
    Ok(membership)
}

fn enroll_code(conn: &Connection, code_hash: &str) -> Result<Option<EnrollCode>> {
    let code = conn
        .query_row(
            "SELECT code_hash, channel_id, expires_at, used_at, created_at
             FROM enroll_codes WHERE code_hash = ?1",
            params![code_hash],
            EnrollCode::from_row,
        )
        .optional()?;
    Ok(code)
}

/// 追加一帧；同 sender + idem_key 重试返回原 seq。
#[allow(clippy::too_many_arguments)]
pub fn append_frame(
    conn: &mut Connection,
    channel_id: &str,
    sender: &str,
    content_type: &str,
    payload: &[u8],
    idem_key: &str,
    default_max_payload: i64,
    now: i64,
) -> Result<AppendOutcome> {
    if idem_key.is_empty() {
        return Err(RelayError::InvalidArgument(
            "idempotency key must not be empty",
        ));
    }
    let tx = conn.transaction()?;
    let channel = get_channel(&tx, channel_id)?.ok_or(RelayError::NotFound)?;
    let max_payload = channel.max_payload_bytes.unwrap_or(default_max_payload);
    let payload_len = i64::try_from(payload.len()).map_err(|_| RelayError::PayloadTooLarge)?;
    if payload_len > max_payload {
        return Err(RelayError::PayloadTooLarge);
    }
    if let Some(existing) = find_idem(&tx, channel_id, sender, idem_key)? {
        if existing.content_type == content_type && existing.payload.as_slice() == payload {
            tx.commit()?;
            return Ok(AppendOutcome {
                seq: existing.seq,
                duplicate: true,
            });
        }
        return Err(RelayError::Conflict(
            "idempotency key reused with different payload".to_owned(),
        ));
    }
    let seq = channel.next_seq;
    tx.execute(
        "INSERT INTO frames (channel_id, seq, sender, content_type, payload, created_at, idem_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            channel_id,
            seq,
            sender,
            content_type,
            payload,
            now,
            idem_key
        ],
    )?;
    tx.execute(
        "UPDATE channels SET next_seq = next_seq + 1, bytes = bytes + ?1 WHERE channel_id = ?2",
        params![payload_len, channel_id],
    )?;
    tx.commit()?;
    Ok(AppendOutcome {
        seq,
        duplicate: false,
    })
}

/// 拉取 `seq > cursor` 的帧；默认排除指定 sender。
pub fn fetch_frames(
    conn: &Connection,
    channel_id: &str,
    cursor: i64,
    limit: usize,
    exclude_sender: Option<&str>,
) -> Result<FramePage> {
    let channel = get_channel(conn, channel_id)?.ok_or(RelayError::NotFound)?;
    let start = cursor.max(channel.retained_from - 1);
    let limit = limit.clamp(1, 1000);
    let mut stmt = conn.prepare(
        "SELECT channel_id, seq, sender, content_type, payload, created_at
         FROM frames
         WHERE channel_id = ?1 AND seq > ?2 AND (?3 IS NULL OR sender != ?3)
         ORDER BY seq LIMIT ?4",
    )?;
    let rows = stmt.query_map(
        params![channel_id, start, exclude_sender, (limit + 1) as i64],
        Frame::from_row,
    )?;
    let mut frames = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = frames.len() > limit;
    frames.truncate(limit);
    let next_cursor = frames.last().map(|frame| frame.seq).unwrap_or(cursor);
    Ok(FramePage {
        frames,
        next_cursor,
        has_more,
        retained_from: channel.retained_from,
    })
}

/// 统计某设备待拉取的帧数与字节数。
pub fn pending_stats(
    conn: &Connection,
    channel_id: &str,
    device_id: &str,
    acked_cursor: i64,
) -> Result<(i64, i64)> {
    let stats = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(LENGTH(payload)), 0)
         FROM frames
         WHERE channel_id = ?1 AND seq > ?2 AND sender != ?3",
        params![channel_id, acked_cursor, device_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(stats)
}

fn find_idem(
    conn: &Connection,
    channel_id: &str,
    sender: &str,
    idem_key: &str,
) -> Result<Option<Frame>> {
    let frame = conn
        .query_row(
            "SELECT channel_id, seq, sender, content_type, payload, created_at
             FROM frames WHERE channel_id = ?1 AND sender = ?2 AND idem_key = ?3",
            params![channel_id, sender, idem_key],
            Frame::from_row,
        )
        .optional()?;
    Ok(frame)
}

fn is_constraint(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(error, _)
            if error.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::util::sha256_hex;

    const CHANNEL: &str = "cccccccccccccccccccccccccccccccc";
    const DEVICE: &str = "dddddddddddddddddddddddddddddddd";
    const NOW: i64 = 1_000;

    fn open() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("relay.db")).unwrap();
        (dir, db)
    }

    fn seed(db: &Db) {
        db.with_conn(|conn| insert_channel(conn, CHANNEL, 4096, Some(3600), Some(1024), NOW))
            .unwrap();
        db.with_conn(|conn| insert_device(conn, DEVICE, &sha256_hex(b"token"), NOW))
            .unwrap();
        db.with_conn(|conn| upsert_membership(conn, CHANNEL, DEVICE, NOW))
            .unwrap();
    }

    #[test]
    fn channel_device_membership_roundtrip() {
        let (_dir, db) = open();
        seed(&db);
        db.with_conn(|conn| {
            let channel = get_channel(conn, CHANNEL)?.unwrap();
            assert_eq!(channel.next_seq, 1);
            assert_eq!(channel.retained_from, 1);
            assert_eq!(channel.bytes, 0);
            let device = device_by_token_hash(conn, &sha256_hex(b"token"))?.unwrap();
            assert_eq!(device.device_id, DEVICE);
            assert!(active_membership(conn, CHANNEL, DEVICE)?.is_some());
            assert_eq!(active_members(conn, CHANNEL)?.len(), 1);
            assert_eq!(ack_membership(conn, CHANNEL, DEVICE, 5)?, 5);
            assert_eq!(ack_membership(conn, CHANNEL, DEVICE, 3)?, 5);
            assert!(revoke_membership(conn, CHANNEL, DEVICE, NOW + 1)?);
            assert!(active_membership(conn, CHANNEL, DEVICE)?.is_none());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn enroll_codes_are_one_time_and_expire() {
        let (_dir, db) = open();
        seed(&db);
        let hash = sha256_hex(b"code");
        db.with_conn(|conn| insert_enroll_code(conn, &hash, CHANNEL, NOW + 10, NOW))
            .unwrap();
        let code = db
            .with_conn(|conn| consume_enroll_code(conn, &hash, NOW))
            .unwrap();
        assert_eq!(code.channel_id, CHANNEL);
        assert!(db
            .with_conn(|conn| consume_enroll_code(conn, &hash, NOW))
            .is_err());

        let expired = sha256_hex(b"expired");
        db.with_conn(|conn| insert_enroll_code(conn, &expired, CHANNEL, NOW, NOW))
            .unwrap();
        assert!(matches!(
            db.with_conn(|conn| consume_enroll_code(conn, &expired, NOW)),
            Err(RelayError::Expired(_))
        ));
        assert_eq!(
            db.with_conn(|conn| delete_expired_enroll_codes(conn, NOW))
                .unwrap(),
            2
        );
    }

    #[test]
    fn append_is_idempotent_and_allocates_seq() {
        let (_dir, db) = open();
        seed(&db);
        let first = db
            .with_conn_mut(|conn| {
                append_frame(conn, CHANNEL, DEVICE, "text/plain", b"a", "k1", 1024, NOW)
            })
            .unwrap();
        assert_eq!(first.seq, 1);
        assert!(!first.duplicate);

        let retry = db
            .with_conn_mut(|conn| {
                append_frame(conn, CHANNEL, DEVICE, "text/plain", b"a", "k1", 1024, NOW)
            })
            .unwrap();
        assert_eq!(retry.seq, 1);
        assert!(retry.duplicate);

        assert!(matches!(
            db.with_conn_mut(|conn| {
                append_frame(conn, CHANNEL, DEVICE, "text/plain", b"b", "k1", 1024, NOW)
            }),
            Err(RelayError::Conflict(_))
        ));

        let second = db
            .with_conn_mut(|conn| {
                append_frame(conn, CHANNEL, DEVICE, "text/plain", b"b", "k2", 1024, NOW)
            })
            .unwrap();
        assert_eq!(second.seq, 2);

        db.with_conn(|conn| {
            let page = fetch_frames(conn, CHANNEL, 0, 10, None)?;
            assert_eq!(page.frames.len(), 2);
            assert_eq!(page.next_cursor, 2);
            assert!(!page.has_more);
            assert_eq!(page.retained_from, 1);
            assert_eq!(page.frames[0].payload, b"a");

            let page = fetch_frames(conn, CHANNEL, 0, 10, Some(DEVICE))?;
            assert!(page.frames.is_empty());
            assert_eq!(page.next_cursor, 0);

            let page = fetch_frames(conn, CHANNEL, 1, 10, None)?;
            assert_eq!(page.frames.len(), 1);
            assert_eq!(page.frames[0].seq, 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn payload_limit_does_not_consume_seq() {
        let (_dir, db) = open();
        seed(&db);
        let payload = vec![0u8; 2048];
        assert!(matches!(
            db.with_conn_mut(|conn| {
                append_frame(
                    conn,
                    CHANNEL,
                    DEVICE,
                    "application/octet-stream",
                    &payload,
                    "big",
                    1024,
                    NOW,
                )
            }),
            Err(RelayError::PayloadTooLarge)
        ));
        let channel = db
            .with_conn(|conn| get_channel(conn, CHANNEL))
            .unwrap()
            .unwrap();
        assert_eq!(channel.next_seq, 1);
        assert_eq!(channel.bytes, 0);
    }
}
