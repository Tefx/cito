//! Owned system SQLite connections. Every connection operation is serialized by Database.
use serde_json::{Map, Number, Value};
use std::ffi::{CStr, CString};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex};

#[link(name = "sqlite3")]
extern "C" {
    fn sqlite3_open_v2(
        path: *const libc::c_char,
        db: *mut *mut libc::c_void,
        flags: i32,
        vfs: *const libc::c_char,
    ) -> i32;
    fn sqlite3_close_v2(db: *mut libc::c_void) -> i32;
    fn sqlite3_errmsg(db: *mut libc::c_void) -> *const libc::c_char;
    fn sqlite3_exec(
        db: *mut libc::c_void,
        sql: *const libc::c_char,
        callback: Option<
            unsafe extern "C" fn(
                *mut libc::c_void,
                i32,
                *mut *mut libc::c_char,
                *mut *mut libc::c_char,
            ) -> i32,
        >,
        arg: *mut libc::c_void,
        error: *mut *mut libc::c_char,
    ) -> i32;
    fn sqlite3_prepare_v2(
        db: *mut libc::c_void,
        sql: *const libc::c_char,
        n: i32,
        statement: *mut *mut libc::c_void,
        tail: *mut *const libc::c_char,
    ) -> i32;
    fn sqlite3_finalize(statement: *mut libc::c_void) -> i32;
    fn sqlite3_step(statement: *mut libc::c_void) -> i32;
    fn sqlite3_bind_null(statement: *mut libc::c_void, index: i32) -> i32;
    fn sqlite3_bind_int64(statement: *mut libc::c_void, index: i32, value: i64) -> i32;
    fn sqlite3_bind_double(statement: *mut libc::c_void, index: i32, value: f64) -> i32;
    fn sqlite3_bind_text(
        statement: *mut libc::c_void,
        index: i32,
        value: *const libc::c_char,
        n: i32,
        destructor: Option<unsafe extern "C" fn(*mut libc::c_void)>,
    ) -> i32;
    fn sqlite3_column_count(statement: *mut libc::c_void) -> i32;
    fn sqlite3_column_name(statement: *mut libc::c_void, column: i32) -> *const libc::c_char;
    fn sqlite3_column_type(statement: *mut libc::c_void, column: i32) -> i32;
    fn sqlite3_column_int64(statement: *mut libc::c_void, column: i32) -> i64;
    fn sqlite3_column_double(statement: *mut libc::c_void, column: i32) -> f64;
    fn sqlite3_column_text(statement: *mut libc::c_void, column: i32) -> *const u8;
    fn sqlite3_column_bytes(statement: *mut libc::c_void, column: i32) -> i32;
    fn sqlite3_changes(db: *mut libc::c_void) -> i32;
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
            visit(Value::Object(row));
        }
        drop(stmt);
        Ok(())
    }
}
#[derive(Clone)]
pub struct Database {
    connection: Arc<Mutex<Connection>>,
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
        };
        db.execute(
            "PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;",
            &[],
        )?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        Ok(db)
    }
    pub fn execute(&self, sql: &str, values: &[Value]) -> Result<usize, String> {
        self.connection
            .lock()
            .map_err(|_| "SQLite lock poisoned")?
            .execute(sql, values)
    }
    pub fn query(&self, sql: &str, values: &[Value]) -> Result<Vec<Value>, String> {
        self.connection
            .lock()
            .map_err(|_| "SQLite lock poisoned")?
            .query(sql, values)
    }
    pub fn transaction<T>(
        &self,
        action: impl FnOnce(&Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        let conn = self.connection.lock().map_err(|_| "SQLite lock poisoned")?;
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
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
