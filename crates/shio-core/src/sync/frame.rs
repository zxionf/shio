//! `SHY2` 帧头布局、编码与统一校验。
//!
//! 帧头固定 77 字节，`frame_len` 不包含自身 4 字节。这里只处理帧头与
//! 长度前缀条目边界；条目内容的业务编码留在 `shio-sync`。

use crate::model::DeviceId;
use crate::sync::{MAX_FRAME_BYTES, MAX_FRAME_ENTRIES};
use crate::{Error, Result};

/// 帧魔数。
pub const MAGIC: &[u8; 4] = b"SHY2";
/// 当前帧格式版本。
pub const FORMAT: u8 = 2;
/// 固定帧头长度。
pub const HEADER_LEN: usize = 77;
/// 变更页。
pub const KIND_CHANGES: u8 = 0;
/// 轮换 offer 页。
pub const KIND_OFFERS: u8 = 1;
/// 快照页。
pub const KIND_SNAPSHOT: u8 = 2;
/// 进度帧。
pub const KIND_PROGRESS: u8 = 3;

const FRAME_LEN_OFFSET: usize = 0;
const MAGIC_OFFSET: usize = 4;
const FORMAT_OFFSET: usize = 8;
const KIND_OFFSET: usize = 9;
const FLAGS_OFFSET: usize = 10;
const DB_OFFSET: usize = 11;
const SENDER_OFFSET: usize = 27;
const AFTER_OFFSET: usize = 43;
const LOG_START_OFFSET: usize = 51;
const LOG_END_OFFSET: usize = 59;
const NEXT_OFFSET: usize = 67;
const COUNT_OFFSET: usize = 75;

/// 解析后的帧头。
#[derive(Debug, Clone, Copy)]
pub struct Header {
    /// 帧类型。
    pub kind: u8,
    /// 逻辑库 ID。
    pub db_id: [u8; 16],
    /// 发送方设备。
    pub sender: DeviceId,
    /// 变更页游标起点。
    pub after: u64,
    /// 发送方日志起点。
    pub log_start: u64,
    /// 发送方日志终点。
    pub log_end: u64,
    /// 下一页游标或进度游标。
    pub next: u64,
    /// 条目数。
    pub count: usize,
}

/// 编码帧头所需的元数据；条目数由编码函数计算。
#[derive(Debug, Clone, Copy)]
pub struct FrameMeta {
    /// 帧类型。
    pub kind: u8,
    /// 逻辑库 ID。
    pub db_id: [u8; 16],
    /// 发送方设备。
    pub sender: DeviceId,
    /// 变更页游标起点。
    pub after: u64,
    /// 发送方日志起点。
    pub log_start: u64,
    /// 发送方日志终点。
    pub log_end: u64,
    /// 下一页游标或进度游标。
    pub next: u64,
}

/// 编码一帧：77 字节头 + 每个条目 `u32` 长度前缀。
pub fn encode_frame(meta: FrameMeta, entries: Vec<Vec<u8>>) -> Result<Vec<u8>> {
    if entries.len() > MAX_FRAME_ENTRIES {
        return Err(Error::InvalidArgument("too many entries in one frame"));
    }
    let count = u16::try_from(entries.len())
        .map_err(|_| Error::InvalidArgument("too many entries in one frame"))?;
    let mut body_len = HEADER_LEN - 4;
    for entry in &entries {
        let len = u32::try_from(entry.len())
            .map_err(|_| Error::InvalidArgument("frame entry is too large"))?;
        body_len = body_len
            .checked_add(4 + len as usize)
            .ok_or(Error::InvalidArgument("frame length overflow"))?;
    }
    let total = body_len
        .checked_add(4)
        .ok_or(Error::InvalidArgument("frame length overflow"))?;
    if total > MAX_FRAME_BYTES {
        return Err(Error::InvalidArgument("frame exceeds the size limit"));
    }
    let frame_len =
        u32::try_from(body_len).map_err(|_| Error::InvalidArgument("frame length overflow"))?;

    let mut frame = Vec::with_capacity(total);
    frame.extend_from_slice(&frame_len.to_be_bytes());
    frame.extend_from_slice(MAGIC);
    frame.push(FORMAT);
    frame.push(meta.kind);
    frame.push(0);
    frame.extend_from_slice(&meta.db_id);
    frame.extend_from_slice(meta.sender.as_bytes());
    frame.extend_from_slice(&meta.after.to_be_bytes());
    frame.extend_from_slice(&meta.log_start.to_be_bytes());
    frame.extend_from_slice(&meta.log_end.to_be_bytes());
    frame.extend_from_slice(&meta.next.to_be_bytes());
    frame.extend_from_slice(&count.to_be_bytes());
    for entry in entries {
        let len = u32::try_from(entry.len())
            .map_err(|_| Error::InvalidArgument("frame entry is too large"))?;
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(&entry);
    }
    Ok(frame)
}

/// 解析帧头与条目边界；不做 `db_id` / `sender` 的身份比对。
pub fn parse_frame(frame: &[u8]) -> Result<(Header, Vec<&[u8]>)> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(Error::InvalidArgument("frame exceeds the size limit"));
    }
    if frame.len() < HEADER_LEN {
        return Err(Error::Corrupted("frame is shorter than the header"));
    }
    let frame_len = u32::from_be_bytes(
        frame[FRAME_LEN_OFFSET..FRAME_LEN_OFFSET + 4]
            .try_into()
            .map_err(|_| Error::Corrupted("frame length is truncated"))?,
    );
    if usize::try_from(frame_len).map_err(|_| Error::Corrupted("frame length overflow"))?
        != frame.len() - 4
    {
        return Err(Error::Corrupted("frame length does not match the buffer"));
    }
    if &frame[MAGIC_OFFSET..MAGIC_OFFSET + 4] != MAGIC {
        return Err(Error::Corrupted("frame magic mismatch"));
    }
    let format = frame[FORMAT_OFFSET];
    if format != FORMAT {
        return Err(Error::UnsupportedFormat {
            found: u16::from(format),
            supported: u16::from(FORMAT),
        });
    }
    let kind = frame[KIND_OFFSET];
    if !matches!(
        kind,
        KIND_CHANGES | KIND_OFFERS | KIND_SNAPSHOT | KIND_PROGRESS
    ) {
        return Err(Error::InvalidArgument("unknown frame kind"));
    }
    if frame[FLAGS_OFFSET] != 0 {
        return Err(Error::Corrupted("reserved frame flags are not zero"));
    }
    let db_id: [u8; 16] = frame[DB_OFFSET..DB_OFFSET + 16]
        .try_into()
        .map_err(|_| Error::Corrupted("frame db id is truncated"))?;
    let sender: [u8; 16] = frame[SENDER_OFFSET..SENDER_OFFSET + 16]
        .try_into()
        .map_err(|_| Error::Corrupted("frame sender is truncated"))?;
    let sender = DeviceId::from_bytes(sender);
    let after = read_u64(frame, AFTER_OFFSET)?;
    let log_start = read_u64(frame, LOG_START_OFFSET)?;
    let log_end = read_u64(frame, LOG_END_OFFSET)?;
    let next = read_u64(frame, NEXT_OFFSET)?;
    let count = usize::from(u16::from_be_bytes(
        frame[COUNT_OFFSET..COUNT_OFFSET + 2]
            .try_into()
            .map_err(|_| Error::Corrupted("frame count is truncated"))?,
    ));
    if count > MAX_FRAME_ENTRIES {
        return Err(Error::InvalidArgument("frame contains too many entries"));
    }

    let mut entries = Vec::with_capacity(count);
    let mut offset = HEADER_LEN;
    for _ in 0..count {
        if offset + 4 > frame.len() {
            return Err(Error::Corrupted("frame entry length is truncated"));
        }
        let len = u32::from_be_bytes(
            frame[offset..offset + 4]
                .try_into()
                .map_err(|_| Error::Corrupted("frame entry length is truncated"))?,
        );
        let len =
            usize::try_from(len).map_err(|_| Error::Corrupted("frame entry length overflow"))?;
        let start = offset + 4;
        let end = start
            .checked_add(len)
            .ok_or(Error::Corrupted("frame entry length overflow"))?;
        if end > frame.len() {
            return Err(Error::Corrupted("frame entry is truncated"));
        }
        entries.push(&frame[start..end]);
        offset = end;
    }
    if offset != frame.len() {
        return Err(Error::Corrupted("frame has trailing bytes"));
    }

    Ok((
        Header {
            kind,
            db_id,
            sender,
            after,
            log_start,
            log_end,
            next,
            count,
        },
        entries,
    ))
}

/// 统一校验入口：解析帧头，并比对期望的 `db_id` 与 `sender`。
///
/// 上传前用 `expected_sender = 本机 device_id`；下载后用 relay 返回的
/// `sender`，不要信任 payload 内的 sender。
pub fn validate_frame(
    frame: &[u8],
    expected_db_id: [u8; 16],
    expected_sender: DeviceId,
) -> Result<(Header, Vec<&[u8]>)> {
    let (header, entries) = parse_frame(frame)?;
    if header.db_id != expected_db_id {
        return Err(Error::InvalidArgument("frame belongs to another database"));
    }
    if header.sender != expected_sender {
        return Err(Error::InvalidArgument("frame sender does not match"));
    }
    Ok((header, entries))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_be_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .map_err(|_| Error::Corrupted("frame field is truncated"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{MAX_FRAME_BYTES, MAX_FRAME_ENTRIES};
    use crate::Error;

    fn meta(kind: u8, sender: DeviceId) -> FrameMeta {
        FrameMeta {
            kind,
            db_id: [7u8; 16],
            sender,
            after: 0,
            log_start: 0,
            log_end: 0,
            next: 0,
        }
    }

    #[test]
    fn validates_expected_db_and_sender() {
        let sender = DeviceId::from_bytes([1u8; 16]);
        let frame = encode_frame(meta(KIND_CHANGES, sender), Vec::new()).unwrap();
        let (header, entries) = validate_frame(&frame, [7u8; 16], sender).unwrap();
        assert_eq!(header.kind, KIND_CHANGES);
        assert_eq!(header.sender, sender);
        assert!(entries.is_empty());

        assert!(matches!(
            validate_frame(&frame, [8u8; 16], sender),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            validate_frame(&frame, [7u8; 16], DeviceId::from_bytes([2u8; 16])),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn rejects_bad_magic_format_kind_and_flags() {
        let sender = DeviceId::from_bytes([1u8; 16]);
        let frame = encode_frame(meta(KIND_PROGRESS, sender), Vec::new()).unwrap();

        let mut bad = frame.clone();
        bad[MAGIC_OFFSET] = b'X';
        assert!(matches!(
            validate_frame(&bad, [7u8; 16], sender),
            Err(Error::Corrupted(_))
        ));

        let mut bad = frame.clone();
        bad[FORMAT_OFFSET] = 3;
        assert!(matches!(
            validate_frame(&bad, [7u8; 16], sender),
            Err(Error::UnsupportedFormat { .. })
        ));

        let mut bad = frame.clone();
        bad[KIND_OFFSET] = 9;
        assert!(matches!(
            validate_frame(&bad, [7u8; 16], sender),
            Err(Error::InvalidArgument(_))
        ));

        let mut bad = frame.clone();
        bad[FLAGS_OFFSET] = 1;
        assert!(matches!(
            validate_frame(&bad, [7u8; 16], sender),
            Err(Error::Corrupted(_))
        ));
    }

    #[test]
    fn rejects_truncated_and_trailing_frames() {
        let sender = DeviceId::from_bytes([1u8; 16]);
        let frame = encode_frame(meta(KIND_CHANGES, sender), Vec::new()).unwrap();

        let truncated = &frame[..HEADER_LEN - 1];
        assert!(matches!(
            validate_frame(truncated, [7u8; 16], sender),
            Err(Error::Corrupted(_))
        ));

        let mut trailing = frame.clone();
        trailing.push(0);
        assert!(matches!(
            validate_frame(&trailing, [7u8; 16], sender),
            Err(Error::Corrupted(_))
        ));
    }

    #[test]
    fn rejects_oversized_and_too_many_entries() {
        let sender = DeviceId::from_bytes([1u8; 16]);
        let oversized = vec![0u8; MAX_FRAME_BYTES + 1];
        assert!(matches!(
            validate_frame(&oversized, [7u8; 16], sender),
            Err(Error::InvalidArgument(_))
        ));

        let entries = vec![Vec::new(); MAX_FRAME_ENTRIES + 1];
        assert!(matches!(
            encode_frame(meta(KIND_CHANGES, sender), entries),
            Err(Error::InvalidArgument(_))
        ));
    }
}
