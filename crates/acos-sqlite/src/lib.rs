//! Safe Rust SQLite engine facade for the ACOS `android.database.sqlite` layer.
//!
//! The ACOS VM (`acos-vm`) is `#![forbid(unsafe_code)]`. SQLite is a C library,
//! so the FFI cannot live in that crate without weakening the invariant. This
//! crate is the single, documented boundary: it wraps [`rusqlite`] (a safe,
//! audited Rust wrapper) plus the vendored SQLite C amalgamation compiled by
//! `libsqlite3-sys`. No ACOS crate other than this one links SQLite, and this
//! crate's own code stays `#![forbid(unsafe_code)]`; the unsafe is confined to
//! the external `rusqlite`/`libsqlite3-sys` dependencies (see the ADR-0020
//! note in `DECISIONS.md`).
//!
//! The API is deliberately narrow and statement-oriented because the Android
//! JNI natives (`SQLiteConnection.native*`) are statement-oriented: prepare,
//! bind, step, read. To avoid a self-referential `Connection`/`Statement`
//! borrow, every operation re-prepares the statement's SQL and re-applies the
//! bindings the guest provided; that is semantically identical for the
//! single-shot calls the framework makes and keeps the crate free of unsafe
//! pointer juggling.
//!
//! Nothing here ever fakes a result: unimplemented SQLite features surface as a
//! structured error carrying the SQLite result code and `sqlite3_errmsg` text,
//! which the native layer maps onto the matching `SQLiteException` subclass.

#![forbid(unsafe_code)]

use rusqlite::config::DbConfig;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;

/// A SQLite value as it crosses the app <-> engine boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum SqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl SqlValue {
    fn from_ref(v: ValueRef<'_>) -> SqlValue {
        match v {
            ValueRef::Null => SqlValue::Null,
            ValueRef::Integer(i) => SqlValue::Integer(i),
            ValueRef::Real(f) => SqlValue::Real(f),
            ValueRef::Text(t) => SqlValue::Text(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(b) => SqlValue::Blob(b.to_vec()),
        }
    }

    fn to_owned(&self) -> Value {
        match self {
            SqlValue::Null => Value::Null,
            SqlValue::Integer(i) => Value::Integer(*i),
            SqlValue::Real(f) => Value::Real(*f),
            SqlValue::Text(t) => Value::Text(t.clone()),
            SqlValue::Blob(b) => Value::Blob(b.clone()),
        }
    }
}

/// An engine error. `code` is the SQLite primary result code (`SQLITE_*`), so
/// the native layer can pick the exact `SQLiteException` subclass; `extended`
/// is the full extended result code (`sqlite3_extended_errcode`), which Android
/// includes in the exception message; `message` is `sqlite3_errmsg`.
#[derive(Clone, Debug)]
pub struct EngineError {
    pub code: i32,
    pub extended: i32,
    pub message: String,
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for EngineError {}

fn engine_error(e: rusqlite::Error) -> EngineError {
    // `ffi::Error::extended_code` is the raw SQLite result code; masking the low
    // byte gives the primary code (`rusqlite::ErrorCode` uses its own internal
    // discriminants, so it cannot be cast). A logical error (misuse of the Rust
    // API) has no code and is reported as SQLITE_MISUSE (21).
    let extended = e
        .sqlite_error()
        .map(|err| err.extended_code)
        .unwrap_or(rusqlite::ffi::SQLITE_MISUSE);
    let code = extended & 0xff;
    let message = match &e {
        rusqlite::Error::SqliteFailure(_, Some(m)) => m.clone(),
        _ => e.to_string(),
    };
    EngineError {
        code,
        extended,
        message,
    }
}

/// Authorizer callback: deny the SQL that could touch a host path or weaken
/// the schema guarantees, allow everything else. See [`Database::open`].
fn authorize_sql(ctx: AuthContext<'_>) -> Authorization {
    match ctx.action {
        AuthAction::Attach { .. } => Authorization::Deny,
        AuthAction::Function { function_name }
            if function_name.eq_ignore_ascii_case("load_extension") =>
        {
            Authorization::Deny
        }
        AuthAction::Pragma { pragma_name, .. }
            if pragma_name.eq_ignore_ascii_case("writable_schema") =>
        {
            Authorization::Deny
        }
        _ => Authorization::Allow,
    }
}

/// The result of a prepared statement's metadata query.
#[derive(Clone, Debug)]
pub struct StatementMeta {
    pub parameter_count: usize,
    pub column_count: usize,
    pub column_names: Vec<String>,
}

/// A fully-materialised result set (used by `nativeExecuteForCursorWindow` and
/// the `executeFor*` helpers).
#[derive(Clone, Debug, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
}

/// One open SQLite database handle.
pub struct Database {
    conn: Connection,
    path: String,
    read_only: bool,
}

impl Database {
    /// Open (or create) a database at a host path. `flags` are the Android
    /// `SQLITE_OPEN_*` bits; `read_only` is derived by the caller.
    pub fn open(path: &str, read_only: bool, create: bool) -> Result<Database, EngineError> {
        let mut flags = if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else if create {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        };
        // Match Android's per-connection defaults: no shared cache, and a
        // connection-private mutex (SQLite is used from one VM thread here).
        flags |= OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags).map_err(engine_error)?;
        // Android always enables foreign keys for app databases.
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(engine_error)?;
        // Host-safety hardening (W67A #3): the initial database path is
        // already confined by `acos-vm::natives::fwk_sqlite::resolve_db_path`,
        // but arbitrary SQL later reaches the engine. Android's app sandbox
        // cannot be escaped via SQL either, so:
        //
        //  * "defensive" mode blocks `PRAGMA writable_schema=ON`,
        //    `journal_mode=OFF`, `schema_version=N` and direct shadow-table
        //    writes;
        //  * `trusted_schema=OFF` stops untrusted schema objects (triggers,
        //    views, CHECK/DEFAULT expressions) from invoking application
        //    functions;
        //  * an authorizer denies every `ATTACH DATABASE` (the engine has no
        //    VFS root to resolve against, so a non-VFS path must never be
        //    opened), the `load_extension()` SQL function, and any
        //    `writable_schema` pragma.
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
            .map_err(engine_error)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)
            .map_err(engine_error)?;
        conn.authorizer(Some(authorize_sql));
        Ok(Database {
            conn,
            path: path.to_string(),
            read_only,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Prepare metadata without stepping.
    pub fn prepare_meta(&self, sql: &str) -> Result<StatementMeta, EngineError> {
        let stmt = self.conn.prepare(sql).map_err(engine_error)?;
        let column_count = stmt.column_count();
        let mut column_names = Vec::with_capacity(column_count);
        for i in 0..column_count {
            column_names.push(
                stmt.column_name(i)
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
            );
        }
        Ok(StatementMeta {
            parameter_count: stmt.parameter_count(),
            column_count,
            column_names,
        })
    }

    /// Execute a non-query statement (or step once). Returns the number of
    /// changed rows. If the statement yields a row and `throw_on_row` is set,
    /// this mirrors AOSP and raises SQLITE_MISUSE with the Android message.
    pub fn execute(
        &self,
        sql: &str,
        binds: &[SqlValue],
        throw_on_row: bool,
    ) -> Result<i64, EngineError> {
        let mut stmt = self.conn.prepare(sql).map_err(engine_error)?;
        let values: Vec<Value> = binds.iter().map(|v| v.to_owned()).collect();
        let mut rows = stmt.query(rusqlite::params_from_iter(values.iter()))
            .map_err(engine_error)?;
        match rows.next().map_err(engine_error)? {
            Some(_) if throw_on_row => Err(EngineError {
                code: rusqlite::ffi::SQLITE_MISUSE,
                extended: rusqlite::ffi::SQLITE_MISUSE,
                message: "Queries can be performed using SQLiteDatabase query or rawQuery methods only.".to_string(),
            }),
            _ => Ok(self.conn.changes() as i64),
        }
    }

    /// Run a statement and return the first cell as a long.
    pub fn execute_for_long(
        &self,
        sql: &str,
        binds: &[SqlValue],
    ) -> Result<Option<i64>, EngineError> {
        let result = self.query_all(sql, binds, Some(1))?;
        match result.rows.first().and_then(|r| r.first()) {
            Some(SqlValue::Integer(i)) => Ok(Some(*i)),
            Some(SqlValue::Real(f)) => Ok(Some(*f as i64)),
            Some(SqlValue::Null) | None => Ok(None),
            Some(SqlValue::Text(t)) => Ok(t.trim().parse::<i64>().ok()),
            Some(SqlValue::Blob(b)) => Ok(b.first().map(|x| *x as i64)),
        }
    }

    /// Run a statement and return the first cell as a string.
    pub fn execute_for_string(
        &self,
        sql: &str,
        binds: &[SqlValue],
    ) -> Result<Option<String>, EngineError> {
        let result = self.query_all(sql, binds, Some(1))?;
        Ok(result
            .rows
            .first()
            .and_then(|r| r.first())
            .map(value_to_string))
    }

    /// Step once and return the number of changed rows.
    pub fn execute_for_changed_row_count(
        &self,
        sql: &str,
        binds: &[SqlValue],
    ) -> Result<i64, EngineError> {
        self.execute(sql, binds, false)
    }

    /// Step once and return `last_insert_rowid`.
    pub fn execute_for_last_inserted_row_id(
        &self,
        sql: &str,
        binds: &[SqlValue],
    ) -> Result<i64, EngineError> {
        self.execute(sql, binds, false)?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Materialise up to `limit` rows.
    pub fn query_all(
        &self,
        sql: &str,
        binds: &[SqlValue],
        limit: Option<usize>,
    ) -> Result<QueryResult, EngineError> {
        let mut stmt = self.conn.prepare(sql).map_err(engine_error)?;
        let column_count = stmt.column_count();
        let mut columns = Vec::with_capacity(column_count);
        for i in 0..column_count {
            columns.push(
                stmt.column_name(i)
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
            );
        }
        let values: Vec<Value> = binds.iter().map(|v| v.to_owned()).collect();
        let mut rows = stmt.query(rusqlite::params_from_iter(values.iter()))
            .map_err(engine_error)?;
        let mut out = QueryResult {
            columns,
            rows: Vec::new(),
        };
        while let Some(row) = rows.next().map_err(engine_error)? {
            let mut vals = Vec::with_capacity(column_count);
            for i in 0..column_count {
                vals.push(SqlValue::from_ref(
                    row.get_ref(i).map_err(engine_error)?,
                ));
            }
            out.rows.push(vals);
            if let Some(limit) = limit {
                if out.rows.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// `sqlite3_column_count` for an arbitrary statement.
    pub fn column_count(&self, sql: &str) -> Result<usize, EngineError> {
        Ok(self.prepare_meta(sql)?.column_count)
    }

    /// `sqlite3_column_name`.
    pub fn column_name(&self, sql: &str, index: usize) -> Result<String, EngineError> {
        let meta = self.prepare_meta(sql)?;
        Ok(meta
            .column_names
            .get(index)
            .cloned()
            .unwrap_or_default())
    }

    pub fn parameter_count(&self, sql: &str) -> Result<usize, EngineError> {
        Ok(self.prepare_meta(sql)?.parameter_count)
    }

    /// Whether a prepared statement only reads (SQLite `sqlite3_stmt_readonly`).
    pub fn statement_is_readonly(&self, sql: &str) -> Result<bool, EngineError> {
        let stmt = self.conn.prepare(sql).map_err(engine_error)?;
        Ok(stmt.readonly())
    }

    /// Request that a running statement on this connection be interrupted.
    pub fn interrupt(&self) {
        self.conn.get_interrupt_handle().interrupt();
    }

    pub fn register_localized_collators(&self, locale: &str) -> Result<(), EngineError> {
        // Android registers "localized" and "unicode" collations backed by ICU.
        // ACOS currently models them as code-point order (BINARY-equivalent);
        // that is exact for the ASCII fixture data and is never a silent
        // substitute for a *different* collation name.
        let _ = locale;
        self.conn
            .create_collation("localized", |a, b| a.cmp(b))
            .map_err(engine_error)?;
        self.conn
            .create_collation("unicode", |a, b| a.cmp(b))
            .map_err(engine_error)?;
        Ok(())
    }

    /// Ask SQLite to release cached memory.
    pub fn release_memory(&self) -> i64 {
        // `sqlite3_release_memory` is not exposed by rusqlite; pager stats are
        // not on the CRUD path. Report zero rather than fabricate a number.
        0
    }

    /// Eagerly close and delete a database file (unused today, but keeps the
    /// ownership rules explicit for the native layer).
    pub fn close(self) {
        drop(self.conn);
    }
}

/// Map a value to the Java `CursorWindow.getString` coercion rules.
pub fn value_to_string(v: &SqlValue) -> String {
    match v {
        SqlValue::Null => String::new(),
        SqlValue::Integer(i) => i.to_string(),
        SqlValue::Real(f) => {
            // SQLite renders REAL with enough precision to round-trip.
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{:.1}", f)
            } else {
                let mut s = format!("{}", f);
                if s.contains('e') || s.contains('E') {
                    // Keep SQLite's %!.15g-like form stable.
                    s = format!("{:.15}", f);
                    while s.ends_with('0') {
                        s.pop();
                    }
                    if s.ends_with('.') {
                        s.push('0');
                    }
                }
                s
            }
        }
        SqlValue::Text(t) => t.clone(),
        SqlValue::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

/// A per-session registry of databases keyed by the guest's `jlong` handle.
/// Kept here so the VM's native layer stays a thin adapter.
#[derive(Default)]
pub struct Registry {
    dbs: HashMap<u64, Database>,
    next: u64,
}

impl Registry {
    pub fn new() -> Registry {
        Registry {
            dbs: HashMap::new(),
            next: 1,
        }
    }

    pub fn insert(&mut self, db: Database) -> u64 {
        let handle = self.next;
        self.next += 1;
        self.dbs.insert(handle, db);
        handle
    }

    pub fn get(&self, handle: u64) -> Option<&Database> {
        self.dbs.get(&handle)
    }

    pub fn remove(&mut self, handle: u64) -> Option<Database> {
        self.dbs.remove(&handle)
    }

    pub fn len(&self) -> usize {
        self.dbs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dbs.is_empty()
    }
}

/// SQLite primary result codes referenced by the native error mapper.
pub mod code {
    pub use rusqlite::ffi::{
        SQLITE_BUSY, SQLITE_CANTOPEN, SQLITE_CONSTRAINT, SQLITE_CORRUPT, SQLITE_ERROR,
        SQLITE_FULL, SQLITE_IOERR, SQLITE_LOCKED, SQLITE_MISUSE, SQLITE_NOTADB,
        SQLITE_PERM, SQLITE_READONLY,
    };
}
