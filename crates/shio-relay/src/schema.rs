//! sqlite schema。

use rusqlite::Connection;

use crate::error::{RelayError, Result};

/// 当前 schema 版本。
pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS channels (
  channel_id        TEXT PRIMARY KEY,
  next_seq          INTEGER NOT NULL,
  retained_from     INTEGER NOT NULL,
  bytes             INTEGER NOT NULL DEFAULT 0,
  quota_bytes       INTEGER NOT NULL,
  frame_ttl_secs    INTEGER,
  max_payload_bytes INTEGER,
  created_at        INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS frames (
  channel_id   TEXT NOT NULL,
  seq          INTEGER NOT NULL,
  sender       TEXT NOT NULL,
  content_type TEXT NOT NULL,
  payload      BLOB NOT NULL,
  created_at   INTEGER NOT NULL,
  idem_key     TEXT,
  PRIMARY KEY (channel_id, seq)
);
CREATE UNIQUE INDEX IF NOT EXISTS frames_idem
  ON frames(channel_id, sender, idem_key) WHERE idem_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS frames_created ON frames(channel_id, created_at);

CREATE TABLE IF NOT EXISTS devices (
  device_id   TEXT PRIMARY KEY,
  token_hash  TEXT NOT NULL UNIQUE,
  created_at  INTEGER NOT NULL,
  last_seen   INTEGER,
  revoked_at  INTEGER
);

CREATE TABLE IF NOT EXISTS memberships (
  channel_id   TEXT NOT NULL,
  device_id    TEXT NOT NULL,
  acked_cursor INTEGER NOT NULL DEFAULT 0,
  created_at   INTEGER NOT NULL,
  revoked_at   INTEGER,
  PRIMARY KEY (channel_id, device_id)
);

CREATE TABLE IF NOT EXISTS enroll_codes (
  code_hash  TEXT PRIMARY KEY,
  channel_id TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at    INTEGER,
  created_at INTEGER NOT NULL
);
"#;

/// 创建或校验 schema。
pub fn init(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(RelayError::Config(format!(
            "database schema version {version} is newer than {SCHEMA_VERSION}"
        )));
    }
    conn.execute_batch(SCHEMA_SQL)?;
    if version < SCHEMA_VERSION {
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_tables_and_sets_version() {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        for table in [
            "channels",
            "frames",
            "devices",
            "memberships",
            "enroll_codes",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing table {table}");
        }
    }

    #[test]
    fn is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        init(&conn).unwrap();
    }
}
