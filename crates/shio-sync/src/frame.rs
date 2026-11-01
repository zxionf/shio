//! `SHY2` 帧编码与解码。
//!
//! 帧头布局、编码与统一校验在 `shio_core::sync::frame`；本模块只负责
//! 变更页 / offer 页 / 快照页 / 进度帧的条目编码与页语义。

use shio_core::codec::{decode_record_key, encode_record_key};
use shio_core::codec2;
use shio_core::store::changes;
use shio_core::sync::frame::{encode_frame, parse_frame};
use shio_core::sync::{
    ChangeEntry, ChangePage, OfferPage, RecordChange, SnapshotPage, SnapshotRecord,
    MAX_FRAME_ENTRIES,
};
use shio_core::{DeviceId, Error, Result};

pub use shio_core::sync::frame::{
    validate_frame, FrameMeta, Header, FORMAT, HEADER_LEN, KIND_CHANGES, KIND_OFFERS,
    KIND_PROGRESS, KIND_SNAPSHOT, MAGIC,
};

pub fn encode_change_page(db_id: [u8; 16], page: &ChangePage) -> Result<Vec<u8>> {
    if page.entries.len() > MAX_FRAME_ENTRIES {
        return Err(Error::InvalidArgument("too many entries in one frame"));
    }
    let mut entries = Vec::with_capacity(page.entries.len());
    for entry in &page.entries {
        if let ChangeEntry::Record(change) = entry {
            validate_record_change(change)?;
        }
        entries.push(changes::encode(entry)?);
    }
    encode_frame(
        FrameMeta {
            kind: KIND_CHANGES,
            db_id,
            sender: page.sender,
            after: page.after,
            log_start: page.log_start,
            log_end: page.log_end,
            next: page.next,
        },
        entries,
    )
}

pub fn decode_change_page(expected_db: [u8; 16], frame: &[u8]) -> Result<ChangePage> {
    let (header, raw_entries) = parse_frame(frame)?;
    decode_change_page_parts(expected_db, header, raw_entries)
}

/// 解码变更页，并校验帧头 `db_id` 与 `sender`。
pub fn decode_change_page_checked(
    expected_db: [u8; 16],
    expected_sender: DeviceId,
    frame: &[u8],
) -> Result<ChangePage> {
    let (header, raw_entries) = validate_frame(frame, expected_db, expected_sender)?;
    decode_change_page_parts(expected_db, header, raw_entries)
}

fn decode_change_page_parts(
    expected_db: [u8; 16],
    header: Header,
    raw_entries: Vec<&[u8]>,
) -> Result<ChangePage> {
    if header.kind != KIND_CHANGES {
        return Err(Error::InvalidArgument("frame is not a change page"));
    }
    if header.db_id != expected_db {
        return Err(Error::InvalidArgument("frame belongs to another database"));
    }
    let mut entries = Vec::with_capacity(raw_entries.len());
    for raw in raw_entries {
        entries.push(changes::decode(raw)?);
    }
    if header.next < header.after || header.next > header.log_end {
        return Err(Error::Corrupted("change page cursor is inconsistent"));
    }
    let has_more = header.next < header.log_end;
    Ok(ChangePage {
        sender: header.sender,
        after: header.after,
        next: header.next,
        has_more,
        log_start: header.log_start,
        log_end: header.log_end,
        entries,
    })
}

/// 进度帧：`sender` 报告自己在接收方日志中的已消费游标（帧头 `next`）。
pub fn encode_progress(db_id: [u8; 16], device: DeviceId, cursor: u64) -> Result<Vec<u8>> {
    encode_frame(
        FrameMeta {
            kind: KIND_PROGRESS,
            db_id,
            sender: device,
            after: 0,
            log_start: 0,
            log_end: 0,
            next: cursor,
        },
        Vec::new(),
    )
}

/// 解码进度帧，返回 `(报告方设备, 已消费游标)`。
pub fn decode_progress(expected_db: [u8; 16], frame: &[u8]) -> Result<(DeviceId, u64)> {
    let (header, entries) = parse_frame(frame)?;
    decode_progress_parts(expected_db, header, entries)
}

/// 解码进度帧，并校验帧头 `db_id` 与 `sender`。
pub fn decode_progress_checked(
    expected_db: [u8; 16],
    expected_sender: DeviceId,
    frame: &[u8],
) -> Result<(DeviceId, u64)> {
    let (header, entries) = validate_frame(frame, expected_db, expected_sender)?;
    decode_progress_parts(expected_db, header, entries)
}

fn decode_progress_parts(
    expected_db: [u8; 16],
    header: Header,
    entries: Vec<&[u8]>,
) -> Result<(DeviceId, u64)> {
    if header.kind != KIND_PROGRESS {
        return Err(Error::InvalidArgument("frame is not a progress report"));
    }
    if header.db_id != expected_db {
        return Err(Error::InvalidArgument("frame belongs to another database"));
    }
    if header.count != 0 || !entries.is_empty() {
        return Err(Error::Corrupted("progress frame carries entries"));
    }
    Ok((header.sender, header.next))
}

pub fn encode_offers(db_id: [u8; 16], page: &OfferPage) -> Result<Vec<u8>> {
    if page.offers.len() > MAX_FRAME_ENTRIES {
        return Err(Error::InvalidArgument("too many offers in one frame"));
    }
    let mut entries = Vec::with_capacity(page.offers.len());
    for offer in &page.offers {
        entries.push(changes::encode(&ChangeEntry::RotationOffer(offer.clone()))?);
    }
    encode_frame(
        FrameMeta {
            kind: KIND_OFFERS,
            db_id,
            sender: page.sender,
            after: 0,
            log_start: 0,
            log_end: 0,
            next: 0,
        },
        entries,
    )
}

pub fn decode_offers(expected_db: [u8; 16], frame: &[u8]) -> Result<OfferPage> {
    let (header, raw_entries) = parse_frame(frame)?;
    decode_offers_parts(expected_db, header, raw_entries)
}

/// 解码 offer 页，并校验帧头 `db_id` 与 `sender`。
pub fn decode_offers_checked(
    expected_db: [u8; 16],
    expected_sender: DeviceId,
    frame: &[u8],
) -> Result<OfferPage> {
    let (header, raw_entries) = validate_frame(frame, expected_db, expected_sender)?;
    decode_offers_parts(expected_db, header, raw_entries)
}

fn decode_offers_parts(
    expected_db: [u8; 16],
    header: Header,
    raw_entries: Vec<&[u8]>,
) -> Result<OfferPage> {
    if header.kind != KIND_OFFERS {
        return Err(Error::InvalidArgument("frame is not an offer page"));
    }
    if header.db_id != expected_db {
        return Err(Error::InvalidArgument("frame belongs to another database"));
    }
    let mut offers = Vec::with_capacity(raw_entries.len());
    for raw in raw_entries {
        match changes::decode(raw)? {
            ChangeEntry::RotationOffer(offer) => offers.push(offer),
            _ => return Err(Error::Corrupted("offer frame contains a non-offer entry")),
        }
    }
    offers.sort_by_key(|offer| (offer.created_at, offer.to));
    Ok(OfferPage {
        sender: header.sender,
        offers,
    })
}

pub fn encode_snapshot(db_id: [u8; 16], page: &SnapshotPage) -> Result<Vec<u8>> {
    encode_snapshot_with_sender(db_id, DeviceId::from_bytes([0u8; 16]), page)
}

/// 编码快照页，并把发送方写进帧头；relay 客户端应使用这个入口。
pub fn encode_snapshot_with_sender(
    db_id: [u8; 16],
    sender: DeviceId,
    page: &SnapshotPage,
) -> Result<Vec<u8>> {
    if page.entries.len() > MAX_FRAME_ENTRIES {
        return Err(Error::InvalidArgument(
            "too many snapshot records in one frame",
        ));
    }
    let mut entries = Vec::with_capacity(page.entries.len());
    for record in &page.entries {
        let key = encode_record_key(&record.namespace, &record.collection, &record.id)?;
        let key_len = u16::try_from(key.len())
            .map_err(|_| Error::InvalidArgument("record key is too long"))?;
        let blob_len = u32::try_from(record.blob.len())
            .map_err(|_| Error::InvalidArgument("record blob is too large"))?;
        let mut entry = Vec::with_capacity(2 + key.len() + 4 + record.blob.len());
        entry.extend_from_slice(&key_len.to_be_bytes());
        entry.extend_from_slice(&key);
        entry.extend_from_slice(&blob_len.to_be_bytes());
        entry.extend_from_slice(&record.blob);
        entries.push(entry);
    }
    encode_frame(
        FrameMeta {
            kind: KIND_SNAPSHOT,
            db_id,
            sender,
            after: 0,
            log_start: page.at_seq,
            log_end: page.at_seq,
            next: u64::from(page.next_key.is_some()),
        },
        entries,
    )
}

pub fn decode_snapshot(expected_db: [u8; 16], frame: &[u8]) -> Result<SnapshotPage> {
    let (header, raw_entries) = parse_frame(frame)?;
    decode_snapshot_parts(expected_db, header, raw_entries)
}

/// 解码快照页，并校验帧头 `db_id` 与 `sender`。
pub fn decode_snapshot_checked(
    expected_db: [u8; 16],
    expected_sender: DeviceId,
    frame: &[u8],
) -> Result<SnapshotPage> {
    let (header, raw_entries) = validate_frame(frame, expected_db, expected_sender)?;
    decode_snapshot_parts(expected_db, header, raw_entries)
}

fn decode_snapshot_parts(
    expected_db: [u8; 16],
    header: Header,
    raw_entries: Vec<&[u8]>,
) -> Result<SnapshotPage> {
    if header.kind != KIND_SNAPSHOT {
        return Err(Error::InvalidArgument("frame is not a snapshot page"));
    }
    if header.db_id != expected_db {
        return Err(Error::InvalidArgument("frame belongs to another database"));
    }
    let mut entries = Vec::new();
    for raw in raw_entries {
        if raw.len() < 2 {
            return Err(Error::Corrupted("snapshot entry is too short"));
        }
        let key_len = usize::from(u16::from_be_bytes([raw[0], raw[1]]));
        let key_end = 2usize
            .checked_add(key_len)
            .ok_or(Error::Corrupted("snapshot key length overflow"))?;
        if key_end + 4 > raw.len() {
            return Err(Error::Corrupted("snapshot entry is truncated"));
        }
        let key = &raw[2..key_end];
        let blob_len = u32::from_be_bytes(
            raw[key_end..key_end + 4]
                .try_into()
                .map_err(|_| Error::Corrupted("snapshot blob length is truncated"))?,
        );
        let blob_start = key_end + 4;
        let blob_end = blob_start
            .checked_add(usize::try_from(blob_len).map_err(|_| Error::Corrupted("blob too large"))?)
            .ok_or(Error::Corrupted("snapshot blob length overflow"))?;
        if blob_end != raw.len() {
            return Err(Error::Corrupted("snapshot entry length mismatch"));
        }
        let (namespace, collection, id) = decode_record_key(key)?;
        codec2::RecordBlob::decode(&raw[blob_start..blob_end])?;
        entries.push(SnapshotRecord {
            namespace,
            collection,
            id,
            blob: raw[blob_start..blob_end].to_vec(),
        });
    }
    let next_key = if header.next != 0 {
        entries
            .last()
            .map(|record| encode_record_key(&record.namespace, &record.collection, &record.id))
            .transpose()?
    } else {
        None
    };
    Ok(SnapshotPage {
        at_seq: header.log_end,
        next_key,
        entries,
    })
}

/// 帧里的记录变更与记录 blob 的 header 是否一致；供应用前的额外校验使用。
pub fn validate_record_change(change: &RecordChange) -> Result<()> {
    let blob = codec2::RecordBlob::decode(&change.blob)?;
    if blob.header.key_id != change.key_id
        || blob.header.rev != change.rev
        || blob.header.updated_at != change.updated_at.as_millis()
        || blob.header.is_deleted() != change.deleted
    {
        return Err(Error::Corrupted(
            "change entry does not match the record blob",
        ));
    }
    Ok(())
}
