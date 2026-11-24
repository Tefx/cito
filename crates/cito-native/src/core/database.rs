//! Owned system SQLite connections. Every connection operation is serialized by Database.
use serde_json::{Map, Number, Value};
use std::ffi::{CStr, CString};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex};

use crate::support::sqlite::*;

extern "C" {
    fn sqlite3_close_v2(db: *mut libc::c_void) -> i32;
    fn sqlite3_errmsg(db: *mut libc::c_void) -> *const libc::c_char;
    fn sqlite3_bind_null(stmt: *mut libc::c_void, index: i32) -> i32;
    fn sqlite3_bind_int64(stmt: *mut libc::c_void, index: i32, value: i64) -> i32;
    fn sqlite3_bind_double(stmt: *mut libc::c_void, index: i32, value: f64) -> i32;
    fn sqlite3_column_type(stmt: *mut libc::c_void, column: i32) -> i32;
    fn sqlite3_column_int64(stmt: *mut libc::c_void, column: i32) -> i64;
    fn sqlite3_column_double(stmt: *mut libc::c_void, column: i32) -> f64;
    fn sqlite3_changes(db: *mut libc::c_void) -> i32;
    fn sqlite3_total_changes(db: *mut libc::c_void) -> i32;
}

pub struct Connection {
    raw: *mut libc::c_void,
}
// SAFETY: Database exclusively owns Connection behind a Mutex, no pointers escape it;
// SQLite is opened FULLMUTEX and all prepared statements are finalized before unlocking.
unsafe impl Send for Connection {}
impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: raw is an owned open handle and statements cannot outlive a method call.
        unsafe {
            sqlite3_close_v2(self.raw);
        }
    }
}
// Binding storage is owned by the statement. Drop finalizes SQLite before Rust
// drops these buffers, including error and early-return paths.
struct Statement(*mut libc::c_void, Vec<String>);
impl Drop for Statement {
    fn drop(&mut self) {
        // SAFETY: each statement has one owner and is finalized exactly once.
        unsafe {
            sqlite3_finalize(self.0);
        }
    }
}
impl Connection {
    fn error(&self) -> String {
        // SAFETY: errmsg is a SQLite-owned NUL-terminated string valid until next operation.
        unsafe {
            CStr::from_ptr(sqlite3_errmsg(self.raw))
                .to_string_lossy()
                .into_owned()
        }
    }
    fn prepare(&self, sql: &str) -> Result<Statement, String> {
        let sql = CString::new(sql).map_err(|_| "SQL contains NUL".to_string())?;
        let mut raw = ptr::null_mut();
        // SAFETY: sql remains live through preparation; out pointer is valid.
        let status =
            unsafe { sqlite3_prepare_v2(self.raw, sql.as_ptr(), -1, &mut raw, ptr::null_mut()) };
        if status != 0 {
            if !raw.is_null() {
                unsafe {
                    sqlite3_finalize(raw);
                }
            }
            return Err(self.error());
        }
        if raw.is_null() {
            return Err("Empty SQL statement".into());
        }
        Ok(Statement(raw, Vec::new()))
    }
    fn bound(&self, stmt: &mut Statement, values: &[Value]) -> Result<(), String> {
        // Text buffers remain stable through finalization because Statement owns them.
        stmt.1 = values
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                _ => v.to_string(),
            })
            .collect();
        for (i, value) in values.iter().enumerate() {
            let idx = i as i32 + 1;
            // SAFETY: stmt is live and all borrowed text buffers outlive statement execution.
            let result = unsafe {
                match value {
                    Value::Null => sqlite3_bind_null(stmt.0, idx),
                    Value::Bool(b) => sqlite3_bind_int64(stmt.0, idx, i64::from(*b)),
                    Value::Number(n) if n.is_i64() => {
                        sqlite3_bind_int64(stmt.0, idx, n.as_i64().unwrap_or(0))
                    }
                    Value::Number(n) => sqlite3_bind_double(stmt.0, idx, n.as_f64().unwrap_or(0.0)),
                    _ => sqlite3_bind_text(
                        stmt.0,
                        idx,
                        stmt.1[i].as_ptr().cast(),
                        stmt.1[i]
                            .len()
                            .try_into()
                            .map_err(|_| "SQL value too large")?,
                        None,
                    ),
                }
            };
            if result != 0 {
                return Err(self.error());
            }
        }
        Ok(())
    }
    pub fn execute(&self, sql: &str, values: &[Value]) -> Result<usize, String> {
        if values.is_empty() {
            let sql = CString::new(sql).map_err(|_| "SQL contains NUL".to_string())?;
            // SAFETY: pointers are valid for call duration; no callback is installed.
            if unsafe {
                sqlite3_exec(
                    self.raw,
                    sql.as_ptr(),
                    None,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            } != 0
            {
                return Err(self.error());
            }
        } else {
            let mut stmt = self.prepare(sql)?;
            self.bound(&mut stmt, values)?;
            // SAFETY: statement and bound storage are live.
            let result = unsafe { sqlite3_step(stmt.0) };
            drop(stmt);
            if result != 101 {
                return Err(self.error());
            }
        }
        // SAFETY: connection remains live.
        Ok(unsafe { sqlite3_changes(self.raw) }.max(0) as usize)
    }
    pub fn query(&self, sql: &str, values: &[Value]) -> Result<Vec<Value>, String> {
        let mut rows = Vec::new();
        self.query_each(sql, values, |row| rows.push(row))?;
        Ok(rows)
    }
    /// Stream rows while the caller holds the connection; callbacks must not reenter Database.
    pub fn query_each(
        &self,
        sql: &str,
        values: &[Value],
        mut visit: impl FnMut(Value),
    ) -> Result<(), String> {
        self.query_while(sql, values, |row| {
            visit(row);
            Ok(true)
        })
    }
    /// Stop stepping as soon as a bounded consumer has filled its page.
    pub fn query_while(
        &self,
        sql: &str,
        values: &[Value],
        mut visit: impl FnMut(Value) -> Result<bool, String>,
    ) -> Result<(), String> {
        let mut stmt = self.prepare(sql)?;
        self.bound(&mut stmt, values)?;
        loop {
            // SAFETY: statement and bindings are live, serialized by Database.
            let result = unsafe { sqlite3_step(stmt.0) };
            if result == 101 {
                break;
            }
            if result != 100 {
                return Err(self.error());
            }
            let mut row = Map::new();
            // SAFETY: column pointers remain valid until the next step; copy each value now.
            unsafe {
                for col in 0..sqlite3_column_count(stmt.0) {
                    let name = CStr::from_ptr(sqlite3_column_name(stmt.0, col))
                        .to_string_lossy()
                        .into_owned();
                    let value = match sqlite3_column_type(stmt.0, col) {
                        1 => Value::Number(Number::from(sqlite3_column_int64(stmt.0, col))),
                        2 => Number::from_f64(sqlite3_column_double(stmt.0, col))
                            .map(Value::Number)
                            .unwrap_or(Value::Null),
                        5 => Value::Null,
                        _ => {
                            let p = sqlite3_column_text(stmt.0, col);
                            let len = sqlite3_column_bytes(stmt.0, col).max(0) as usize;
                            if p.is_null() {
                                Value::String(String::new())
                            } else {
                                Value::String(
                                    String::from_utf8_lossy(std::slice::from_raw_parts(p, len))
                                        .into_owned(),
                                )
                            }
                        }
                    };
                    row.insert(name, value);
                }
            }
            if !visit(Value::Object(row))? {
                break;
            }
        }
        drop(stmt);
        Ok(())
    }
}
type CommitNotifier = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct Database {
    connection: Arc<Mutex<Connection>>,
    committed: Arc<Mutex<Option<CommitNotifier>>>,
}
impl Database {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let filename = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| "SQLite path contains NUL".to_string())?;
        let mut raw = ptr::null_mut();
        // SAFETY: filename and out-pointer are valid. FULLMUTEX permits serialized cross-thread ownership.
        let status = unsafe {
            sqlite3_open_v2(
                filename.as_ptr(),
                &mut raw,
                0x00000002 | 0x00000004 | 0x00010000,
                ptr::null(),
            )
        };
        if raw.is_null() {
            return Err("Cannot allocate SQLite connection".into());
        }
        let conn = Connection { raw };
        if status != 0 {
            return Err(conn.error());
        }
        let db = Self {
            connection: Arc::new(Mutex::new(conn)),
            committed: Arc::new(Mutex::new(None)),
        };
        db.execute(
            "PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;",
            &[],
        )?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        Ok(db)
    }
    pub fn set_commit_notifier(
        &self,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Result<(), String> {
        *self
            .committed
            .lock()
            .map_err(|_| "Commit notifier lock poisoned")? = Some(Arc::new(notify));
        Ok(())
    }
    fn notify_committed(&self) {
        let notify = self.committed.lock().ok().and_then(|notify| notify.clone());
        if let Some(notify) = notify {
            notify();
        }
    }
    pub fn execute(&self, sql: &str, values: &[Value]) -> Result<usize, String> {
        let conn = self.connection.lock().map_err(|_| "SQLite lock poisoned")?;
        // SAFETY: the connection lock owns SQLite throughout both counter reads.
        let before = unsafe { sqlite3_total_changes(conn.raw) };
        let result = conn.execute(sql, values);
        let changed = unsafe { sqlite3_total_changes(conn.raw) } != before;
        drop(conn);
        if changed {
            self.notify_committed();
        }
        result
    }
    /// Connection-local write counter; reading it does not execute SQL.
    pub fn mutation_revision(&self) -> Result<i32, String> {
        let conn = self.connection.lock().map_err(|_| "SQLite lock poisoned")?;
        // SAFETY: the lock owns the live SQLite handle throughout the counter read.
        Ok(unsafe { sqlite3_total_changes(conn.raw) })
    }
    pub fn query(&self, sql: &str, values: &[Value]) -> Result<Vec<Value>, String> {
        self.read(|conn| conn.query(sql, values))
    }
    /// Serialize reads on this connection without reserving the SQLite writer.
    /// The callback must only query and must not reenter this Database.
    pub(crate) fn read<T>(
        &self,
        action: impl FnOnce(&Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        let conn = self.connection.lock().map_err(|_| "SQLite lock poisoned")?;
        action(&conn)
    }
    pub fn transaction<T>(
        &self,
        action: impl FnOnce(&Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        self.transaction_notifying(action, true)
    }
    /// Persist diagnostics without scheduling more work from their own commit.
    pub(super) fn diagnostic_transaction<T>(
        &self,
        action: impl FnOnce(&Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        self.transaction_notifying(action, false)
    }
    fn transaction_notifying<T>(
        &self,
        action: impl FnOnce(&Connection) -> Result<T, String>,
        notify: bool,
    ) -> Result<T, String> {
        let conn = self.connection.lock().map_err(|_| "SQLite lock poisoned")?;
        // SAFETY: the connection lock keeps the SQLite handle exclusively owned.
        let before = unsafe { sqlite3_total_changes(conn.raw) };
        conn.execute("BEGIN IMMEDIATE", &[])?;
        struct Rollback<'a>(&'a Connection, bool);
        impl Drop for Rollback<'_> {
            fn drop(&mut self) {
                if !self.1 {
                    let _ = self.0.execute("ROLLBACK", &[]);
                }
            }
        }
        let mut rollback = Rollback(&conn, false);
        let result = action(&conn)?;
        conn.execute("COMMIT", &[])?;
        rollback.1 = true;
        let changed = unsafe { sqlite3_total_changes(conn.raw) } != before;
        drop(rollback);
        drop(conn);
        if changed && notify {
            self.notify_committed();
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::ReadOnlySqlite;
    use serde_json::json;
    use std::time::Duration;
    #[test]
    fn read_only_sqlite_persistent_cantopen_returns_the_cause_not_deadline() -> Result<(), String> {
        let root =
            std::env::temp_dir().join(format!("cito-reader-{}", crate::plugins::random_uuid_v4()));
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let result = (|| {
            let path = root.join("source.sqlite");
            drop(crate::database::Database::open(&path)?);
            let reader = ReadOnlySqlite::open(&path, Duration::from_millis(1200))?;
            let missing = root.join("missing/attached.sqlite");
            let error = reader
                .rows(&format!(
                    "ATTACH DATABASE '{}' AS missing",
                    missing.display()
                ))
                .unwrap_err();
            assert_eq!(
                error,
                "Session database is unavailable; check its location and permissions"
            );
            assert_eq!(reader.rows("SELECT 7 AS value")?[0]["value"], "7");
            Ok(())
        })();
        std::fs::remove_dir_all(root).map_err(|e| e.to_string())?;
        result
    }
    #[test]
    fn read_only_sqlite_initializes_wal_without_shm_and_preserves_committed_frames(
    ) -> Result<(), String> {
        let root =
            std::env::temp_dir().join(format!("cito-reader-{}", crate::plugins::random_uuid_v4()));
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let result = (|| {
            let source = root.join("writer.sqlite");
            let writer = crate::database::Database::open(&source)?;
            writer.execute("PRAGMA journal_mode=WAL; CREATE TABLE items(value TEXT); INSERT INTO items VALUES('checkpointed'); PRAGMA wal_checkpoint(TRUNCATE)",&[])?;
            for nonempty in [false, true] {
                if nonempty {
                    writer.execute("INSERT INTO items VALUES('committed frame')", &[])?;
                }
                let path = root.join(format!("copy #?%{nonempty}.sqlite"));
                std::fs::copy(&source, &path).map_err(|e| e.to_string())?;
                if nonempty {
                    std::fs::copy(
                        source.with_extension("sqlite-wal"),
                        path.with_extension("sqlite-wal"),
                    )
                    .map_err(|e| e.to_string())?;
                }
                assert!(!path.with_extension("sqlite-shm").exists());
                let reader = ReadOnlySqlite::open(&path, Duration::from_secs(1))?;
                let rows = reader.rows("SELECT value FROM items ORDER BY rowid")?;
                assert_eq!(rows.len(), if nonempty { 2 } else { 1 });
                assert_eq!(rows[0]["value"], "checkpointed");
                if nonempty {
                    assert_eq!(rows[1]["value"], "committed frame");
                }
                assert!(reader.rows("PRAGMA table_info(absent)")?.is_empty());
            }
            let limited = ReadOnlySqlite::open(&source, Duration::from_millis(30))?;
            assert_eq!(limited.rows("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n").unwrap_err(),"Session database deadline exceeded");
            assert_eq!(
                ReadOnlySqlite::open(&root.join("absent.sqlite"), Duration::from_millis(30))
                    .err()
                    .ok_or("Expected missing DB")?,
                "Session database was not found"
            );
            let invalid = root.join("invalid.sqlite");
            std::fs::write(&invalid, "not a database").map_err(|e| e.to_string())?;
            let failure = ReadOnlySqlite::open(&invalid, Duration::from_millis(30))
                .err()
                .ok_or("Expected invalid DB error")?;
            assert_eq!(failure, "Session database is invalid");
            assert!(!failure.contains("SQLite") && !failure.contains("not a database"));
            Ok(())
        })();
        std::fs::remove_dir_all(root).map_err(|e| e.to_string())?;
        result
    }
    #[test]
    fn rollback_and_utf8_bindings() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("cito-sqlite-{}", std::process::id()));
        let db = Database::open(&dir.join("state.sqlite"))?;
        db.execute(
            "CREATE TABLE IF NOT EXISTS probe (id INTEGER PRIMARY KEY, text TEXT)",
            &[],
        )?;
        db.execute("DELETE FROM probe", &[])?;
        let result: Result<(), String> = db.transaction(|c| {
            c.execute(
                "INSERT INTO probe VALUES (?,?)",
                &[json!(1), json!("🦀\u{0}tail")],
            )?;
            Err("abort".into())
        });
        assert!(result.is_err());
        assert!(db.query("SELECT * FROM probe", &[])?.is_empty());
        db.execute(
            "INSERT INTO probe VALUES (?,?)",
            &[json!(2), json!("🦀\u{0}tail")],
        )?;
        assert_eq!(
            db.query("SELECT * FROM probe", &[])?[0]["text"],
            json!("🦀\u{0}tail")
        );
        drop(db);
        std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
        Ok(())
    }
}
