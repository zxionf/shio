//! sqlite 连接与事务辅助。

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::{RelayError, Result};

/// 共享 sqlite 连接。
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// 打开数据库，启用 WAL、外键与 busy timeout。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    RelayError::Config(format!("create {}: {err}", parent.display()))
                })?;
            }
        }
        let conn = Connection::open(path)?;
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(RelayError::Config(format!(
                "failed to enable WAL (journal_mode = {mode})"
            )));
        }
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        crate::schema::init(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 只读借用连接。
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| RelayError::InvalidArgument("database mutex poisoned"))?;
        f(&conn)
    }

    /// 可写借用连接。
    pub fn with_conn_mut<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| RelayError::InvalidArgument("database mutex poisoned"))?;
        f(&mut conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_wal_database_and_creates_parent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("relay.db");
        let db = Db::open(&path).unwrap();
        let mode: String = db
            .with_conn(|conn| Ok(conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        assert!(path.exists());
    }
}
