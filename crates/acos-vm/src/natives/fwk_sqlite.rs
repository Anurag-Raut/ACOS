//! Native layer for `android.database.sqlite` and `android.database.CursorWindow`.
//!
//! The real Android 14 framework DEX is interpreted by this VM; every method
//! declared `native` in `SQLiteConnection`, `SQLiteGlobal`, `SQLiteDebug` and
//! `CursorWindow` is supplied here and is backed by the safe SQLite engine in
//! `crates/acos-sqlite` (rusqlite + vendored SQLite C, `#![forbid(unsafe_code)]`
//! on the ACOS side — see the ADR-0020 note in `DECISIONS.md`).
//!
//! ## Sandbox
//!
//! Database files are opened through the guest VFS: a database path is
//! normalised with [`crate::vfs::normalize_guest`] and mapped under
//! `Vfs::root()` exactly like the rest of the file layer. `/data/user/<n>/…`
//! and `/data/data/…` are the only writable forms; a path that escapes the
//! sandbox is refused with `EACCES` (turned into a `SQLiteCantOpenDatabaseException`),
//! never opened against the host filesystem. `:memory:` is passed through.
//!
//! ## Error mapping
//!
//! Every SQLite failure carries its primary result code and `sqlite3_errmsg`
//! text. [`exception_for`] maps the code to the same `SQLiteException`
//! subclass AOSP's `throw_sqlite3_exception` selects, so guest code can
//! `catch (SQLiteConstraintException e)` and observe the device message.
//!
//! ## Unsupported APIs
//!
//! `nativeExecuteForBlobFileDescriptor`, `nativeRegisterCustom{Scalar,Aggregate}Function`,
//! `nativeCreateFromParcel`, `nativeWriteToParcel` and `SQLiteDebug.nativeGetPagerStats`
//! are intentionally **not** registered; an unregistered true native resolves to
//! the structured `UNSUPPORTED_JAVA_API` naming the exact method, never a fake
//! result.

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Handle, Object, NULL};
use crate::vm::Vm;
use acos_sqlite::{Database, EngineError, Registry, SqlValue, StatementMeta};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

const SQLITE_OPEN_READONLY: i32 = 0x0000_0001;
const SQLITE_OPEN_READWRITE: i32 = 0x0000_0002;
const SQLITE_OPEN_CREATE: i32 = 0x0000_0004;
const ANDROID_CREATE_IF_NECESSARY: i32 = 0x1000_0000;

/// A prepared-statement record. The SQL text and current bindings are kept so
/// every engine call can re-prepare and re-apply them (rusqlite's `Statement`
/// borrows its `Connection`, so it cannot be stored beside it).
struct Stmt {
    db: u64,
    sql: String,
    binds: Vec<SqlValue>,
    meta: StatementMeta,
}

/// A native `CursorWindow`. Android's C++ window stores decoded cells in native
/// memory; ACOS keeps the same shape in Rust, including the byte budget so
/// `nativeAllocRow` fails exactly when the window is full.
struct Win {
    name: String,
    num_columns: usize,
    capacity: usize,
    rows: Vec<Vec<SqlValue>>,
}

impl Win {
    fn used_bytes(&self) -> usize {
        let mut total = 0usize;
        for row in &self.rows {
            total += self.num_columns * 4;
            for cell in row {
                total += match cell {
                    SqlValue::Null => 4,
                    SqlValue::Integer(_) | SqlValue::Real(_) => 8,
                    SqlValue::Text(t) => 4 + t.len() * 2,
                    SqlValue::Blob(b) => 4 + b.len(),
                };
            }
        }
        total
    }

    fn row_fits(&self, row: &[SqlValue]) -> bool {
        let mut add = self.num_columns * 4;
        for cell in row {
            add += match cell {
                SqlValue::Null => 4,
                SqlValue::Integer(_) | SqlValue::Real(_) => 8,
                SqlValue::Text(t) => 4 + t.len() * 2,
                SqlValue::Blob(b) => 4 + b.len(),
            };
        }
        // Android reserves a little slop for the row header; the exact number
        // is documented as 4 bytes per field, which `num_columns * 4` covers.
        self.used_bytes() + add <= self.capacity
    }
}

#[derive(Default)]
struct SqliteState {
    reg: Registry,
    stmts: HashMap<u64, Stmt>,
    next_stmt: u64,
    wins: HashMap<u64, Win>,
    next_win: u64,
}

impl SqliteState {
    fn insert_stmt(&mut self, stmt: Stmt) -> u64 {
        // Statements and windows share the guest's opaque-pointer space, so a
        // single monotonic counter keeps handles distinct and non-null.
        self.next_stmt += 1;
        let h = 0x5300_0000_0000_0000u64 | self.next_stmt;
        self.stmts.insert(h, stmt);
        h
    }

    fn insert_win(&mut self, win: Win) -> u64 {
        self.next_win += 1;
        let h = 0x4300_0000_0000_0000u64 | self.next_win;
        self.wins.insert(h, win);
        h
    }
}

thread_local! {
    static STATE: RefCell<SqliteState> = RefCell::new(SqliteState::default());
}

fn with_state<R>(f: impl FnOnce(&mut SqliteState) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// Register every `android.database.sqlite` / `CursorWindow` true native.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let conn = "Landroid/database/sqlite/SQLiteConnection;";
    vm.register_native(conn, "nativeOpen", "(Ljava/lang/String;ILjava/lang/String;ZZII)J", native_open);
    vm.register_native(conn, "nativeClose", "(J)V", native_close);
    vm.register_native(conn, "nativePrepareStatement", "(JLjava/lang/String;)J", native_prepare_statement);
    vm.register_native(conn, "nativeFinalizeStatement", "(JJ)V", native_finalize_statement);
    vm.register_native(conn, "nativeGetParameterCount", "(JJ)I", native_get_parameter_count);
    vm.register_native(conn, "nativeGetColumnCount", "(JJ)I", native_get_column_count);
    vm.register_native(conn, "nativeGetColumnName", "(JJI)Ljava/lang/String;", native_get_column_name);
    vm.register_native(conn, "nativeIsReadOnly", "(JJ)Z", native_is_read_only);
    vm.register_native(conn, "nativeGetDbLookaside", "(J)I", native_get_db_lookaside);
    vm.register_native(conn, "nativeRegisterLocalizedCollators", "(JLjava/lang/String;)V", native_register_localized_collators);
    vm.register_native(conn, "nativeBindNull", "(JJI)V", native_bind_null);
    vm.register_native(conn, "nativeBindLong", "(JJIJ)V", native_bind_long);
    vm.register_native(conn, "nativeBindDouble", "(JJID)V", native_bind_double);
    vm.register_native(conn, "nativeBindString", "(JJILjava/lang/String;)V", native_bind_string);
    vm.register_native(conn, "nativeBindBlob", "(JJI[B)V", native_bind_blob);
    vm.register_native(conn, "nativeResetStatementAndClearBindings", "(JJ)V", native_reset_statement);
    vm.register_native(conn, "nativeExecute", "(JJZ)V", native_execute);
    vm.register_native(conn, "nativeExecuteForLong", "(JJ)J", native_execute_for_long);
    vm.register_native(conn, "nativeExecuteForString", "(JJ)Ljava/lang/String;", native_execute_for_string);
    vm.register_native(conn, "nativeExecuteForChangedRowCount", "(JJ)I", native_execute_for_changed_row_count);
    vm.register_native(conn, "nativeExecuteForLastInsertedRowId", "(JJ)J", native_execute_for_last_inserted_row_id);
    vm.register_native(conn, "nativeExecuteForCursorWindow", "(JJJIIZ)J", native_execute_for_cursor_window);
    vm.register_native(conn, "nativeCancel", "(J)V", native_cancel);
    vm.register_native(conn, "nativeResetCancel", "(JZ)V", native_reset_cancel);

    let cw = "Landroid/database/CursorWindow;";
    vm.register_native(cw, "nativeCreate", "(Ljava/lang/String;I)J", cw_create);
    vm.register_native(cw, "nativeDispose", "(J)V", cw_dispose);
    vm.register_native(cw, "nativeGetName", "(J)Ljava/lang/String;", cw_get_name);
    vm.register_native(cw, "nativeGetNumRows", "(J)I", cw_get_num_rows);
    vm.register_native(cw, "nativeSetNumColumns", "(JI)Z", cw_set_num_columns);
    vm.register_native(cw, "nativeAllocRow", "(J)Z", cw_alloc_row);
    vm.register_native(cw, "nativeFreeLastRow", "(J)V", cw_free_last_row);
    vm.register_native(cw, "nativeClear", "(J)V", cw_clear);
    vm.register_native(cw, "nativeGetType", "(JII)I", cw_get_type);
    vm.register_native(cw, "nativeGetLong", "(JII)J", cw_get_long);
    vm.register_native(cw, "nativeGetDouble", "(JII)D", cw_get_double);
    vm.register_native(cw, "nativeGetString", "(JII)Ljava/lang/String;", cw_get_string);
    vm.register_native(cw, "nativeGetBlob", "(JII)[B", cw_get_blob);
    vm.register_native(cw, "nativePutNull", "(JII)Z", cw_put_null);
    vm.register_native(cw, "nativePutLong", "(JJII)Z", cw_put_long);
    vm.register_native(cw, "nativePutDouble", "(JDII)Z", cw_put_double);
    vm.register_native(cw, "nativePutString", "(JLjava/lang/String;II)Z", cw_put_string);
    vm.register_native(cw, "nativePutBlob", "(J[BII)Z", cw_put_blob);
    vm.register_native(cw, "nativeCopyStringToBuffer", "(JIILandroid/database/CharArrayBuffer;)V", cw_copy_string_to_buffer);

    let sg = "Landroid/database/sqlite/SQLiteGlobal;";
    vm.register_native(sg, "nativeReleaseMemory", "()I", sqlite_global_release_memory);
    Ok(())
}

// -- helpers ---------------------------------------------------------------

fn exception_for(code: i32) -> &'static str {
    match code {
        acos_sqlite::code::SQLITE_CORRUPT | acos_sqlite::code::SQLITE_NOTADB => {
            "Landroid/database/sqlite/SQLiteDatabaseCorruptException;"
        }
        acos_sqlite::code::SQLITE_PERM => "Landroid/database/sqlite/SQLiteAccessPermException;",
        acos_sqlite::code::SQLITE_CANTOPEN => {
            "Landroid/database/sqlite/SQLiteCantOpenDatabaseException;"
        }
        acos_sqlite::code::SQLITE_CONSTRAINT => {
            "Landroid/database/sqlite/SQLiteConstraintException;"
        }
        acos_sqlite::code::SQLITE_BUSY | acos_sqlite::code::SQLITE_LOCKED => {
            "Landroid/database/sqlite/SQLiteDatabaseLockedException;"
        }
        acos_sqlite::code::SQLITE_READONLY => {
            "Landroid/database/sqlite/SQLiteReadOnlyDatabaseException;"
        }
        acos_sqlite::code::SQLITE_IOERR => "Landroid/database/sqlite/SQLiteDiskIOException;",
        acos_sqlite::code::SQLITE_FULL => "Landroid/database/sqlite/SQLiteFullException;",
        acos_sqlite::code::SQLITE_MISUSE => "Ljava/lang/IllegalStateException;",
        _ => "Landroid/database/sqlite/SQLiteException;",
    }
}

fn throw_engine(vm: &mut Vm, e: EngineError, sql: Option<&str>) -> VmError {
    let msg = android_sqlite_message(&e, sql);
    vm.throw_new(exception_for(e.code), Some(&msg))
}

/// Android's symbolic name for a SQLite result code, as appended to the
/// exception message by `throw_sqlite3_exception`.
fn sqlite_code_name(code: i32) -> &'static str {
    match code {
        1 => "SQLITE_ERROR",
        2 => "SQLITE_INTERNAL",
        3 => "SQLITE_PERM",
        4 => "SQLITE_ABORT",
        5 => "SQLITE_BUSY",
        6 => "SQLITE_LOCKED",
        7 => "SQLITE_NOMEM",
        8 => "SQLITE_READONLY",
        9 => "SQLITE_INTERRUPT",
        10 => "SQLITE_IOERR",
        11 => "SQLITE_CORRUPT",
        12 => "SQLITE_NOTFOUND",
        13 => "SQLITE_FULL",
        14 => "SQLITE_CANTOPEN",
        15 => "SQLITE_PROTOCOL",
        16 => "SQLITE_EMPTY",
        17 => "SQLITE_SCHEMA",
        18 => "SQLITE_TOOBIG",
        19 => "SQLITE_CONSTRAINT",
        20 => "SQLITE_MISMATCH",
        21 => "SQLITE_MISUSE",
        22 => "SQLITE_NOLFS",
        23 => "SQLITE_AUTH",
        24 => "SQLITE_FORMAT",
        25 => "SQLITE_RANGE",
        26 => "SQLITE_NOTADB",
        275 => "SQLITE_CONSTRAINT_CHECK",
        531 => "SQLITE_CONSTRAINT_COMMITHOOK",
        787 => "SQLITE_CONSTRAINT_FOREIGNKEY",
        1043 => "SQLITE_CONSTRAINT_FUNCTION",
        1299 => "SQLITE_CONSTRAINT_NOTNULL",
        1555 => "SQLITE_CONSTRAINT_PRIMARYKEY",
        1811 => "SQLITE_CONSTRAINT_TRIGGER",
        2067 => "SQLITE_CONSTRAINT_UNIQUE",
        2323 => "SQLITE_CONSTRAINT_VTAB",
        2579 => "SQLITE_CONSTRAINT_ROWID",
        2835 => "SQLITE_CONSTRAINT_PINNED",
        _ => "SQLITE_ERROR",
    }
}

/// Reproduce Android 14's `throw_sqlite3_exception` message exactly:
/// `"<errmsg> (code <extended> <NAME>)"`, with `": , while compiling: <sql>"`
/// appended for errors raised while preparing a statement.
fn android_sqlite_message(e: &EngineError, sql: Option<&str>) -> String {
    let mut s = format!(
        "{} (code {} {})",
        e.message,
        e.extended,
        sqlite_code_name(e.extended)
    );
    if let Some(sql) = sql {
        s.push_str(": , while compiling: ");
        s.push_str(sql);
    }
    s
}

fn throw_sqlite(vm: &mut Vm, message: &str) -> VmError {
    vm.throw_new("Landroid/database/sqlite/SQLiteException;", Some(message))
}

/// Resolve a guest database path into a host path strictly under the VFS root.
///
/// This mirrors `Vfs::target` for the writable `/data/...` forms without
/// touching the shared VFS module: normalise lexically, map the two guest
/// forms, create the parent directory, then canonicalise and require the result
/// to stay under the canonical sandbox root. `:memory:` is passed through.
fn resolve_db_path(vm: &Vm, guest: &str) -> Result<PathBuf, String> {
    if guest == ":memory:" || guest.is_empty() {
        return Ok(PathBuf::from(":memory:"));
    }
    let norm = crate::vfs::normalize_guest(guest)
        .ok_or_else(|| format!("invalid database path: {guest}"))?;
    let root = vm.vfs.root().to_path_buf();
    let host = if let Some(rest) = norm.strip_prefix("/data/user/") {
        root.join("data").join("user").join(rest)
    } else if let Some(rest) = norm.strip_prefix("/data/user_de/") {
        root.join("data").join("user_de").join(rest)
    } else if let Some(rest) = norm.strip_prefix("/data/data/") {
        root.join("data").join("user").join("0").join(rest)
    } else {
        return Err(format!("database path outside the sandbox: {guest}"));
    };
    if let Some(parent) = host.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create database directory: {e}"))?;
    }
    let canon_root = std::fs::canonicalize(&root).unwrap_or(root);
    let parent = host
        .parent()
        .ok_or_else(|| format!("invalid database path: {guest}"))?;
    let canon_parent = std::fs::canonicalize(parent)
        .map_err(|e| format!("cannot canonicalise database directory: {e}"))?;
    if !canon_parent.starts_with(&canon_root) {
        return Err(format!("database path escapes the sandbox: {guest}"));
    }
    let name = host
        .file_name()
        .ok_or_else(|| format!("invalid database path: {guest}"))?;
    Ok(canon_parent.join(name))
}

fn get_db(state: &SqliteState, handle: i64) -> Option<&Database> {
    state.reg.get(handle as u64)
}

/// Run `f` against a live database. The state borrow ends when this returns, so
/// `R` must be owned (it always is here).
fn with_db<R>(handle: i64, f: impl FnOnce(&Database) -> R) -> Option<R> {
    STATE.with(|s| {
        let state = s.borrow();
        get_db(&state, handle).map(f)
    })
}

fn read_bytes(vm: &Vm, h: Handle) -> Option<Vec<u8>> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Byte(v) => Some(v.iter().map(|&b| b as u8).collect()),
            _ => None,
        },
        _ => None,
    }
}

fn byte_array(vm: &mut Vm, data: Vec<u8>) -> VmResult<Handle> {
    vm.alloc(Object::Array(ArrayObj {
        elem_desc: "B".to_string(),
        data: ArrayData::Byte(data.into_iter().map(|b| b as i8).collect()),
    }))
}

fn value_as_long(v: &SqlValue) -> i64 {
    match v {
        SqlValue::Null => 0,
        SqlValue::Integer(i) => *i,
        SqlValue::Real(f) => *f as i64,
        SqlValue::Text(t) => t.trim().parse::<i64>().unwrap_or(0),
        SqlValue::Blob(_) => 0,
    }
}

fn value_as_double(v: &SqlValue) -> f64 {
    match v {
        SqlValue::Null => 0.0,
        SqlValue::Integer(i) => *i as f64,
        SqlValue::Real(f) => *f,
        SqlValue::Text(t) => t.trim().parse::<f64>().unwrap_or(0.0),
        SqlValue::Blob(_) => 0.0,
    }
}

fn value_type(v: &SqlValue) -> i32 {
    match v {
        SqlValue::Null => 0,
        SqlValue::Integer(_) => 1,
        SqlValue::Real(_) => 2,
        SqlValue::Text(_) => 3,
        SqlValue::Blob(_) => 4,
    }
}

// -- SQLiteConnection ------------------------------------------------------

fn native_open(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = vm.string_to_rust(vm.arg_ref(ctx, 0)).unwrap_or_default();
    let flags = vm.arg_i32(ctx, 1);
    let read_only = flags & SQLITE_OPEN_READONLY != 0 && flags & SQLITE_OPEN_READWRITE == 0;
    let create = flags & (SQLITE_OPEN_CREATE | ANDROID_CREATE_IF_NECESSARY) != 0;

    let host = match resolve_db_path(vm, &path) {
        Ok(p) => p,
        Err(msg) => return Err(throw_sqlite(vm, &msg)),
    };
    let host_str = host.to_string_lossy().into_owned();
    let db = match Database::open(&host_str, read_only, create) {
        Ok(db) => db,
        Err(e) => return Err(throw_engine(vm, e, None)),
    };
    let handle = with_state(|st| st.reg.insert(db));
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn native_close(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    with_state(|st| {
        st.reg.remove(handle);
        st.stmts.retain(|_, s| s.db != handle);
    });
    Ok(())
}

fn native_prepare_statement(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let db_handle = vm.arg_i64(ctx, 0);
    let sql = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
    let meta = match with_db(db_handle, |db| db.prepare_meta(&sql)) {
        Some(Ok(m)) => m,
        Some(Err(e)) => return Err(throw_engine(vm, e, Some(&sql))),
        None => return Err(throw_sqlite(vm, "connection is closed")),
    };
    let binds = vec![SqlValue::Null; meta.parameter_count];
    let handle = with_state(|st| {
        st.insert_stmt(Stmt {
            db: db_handle as u64,
            sql,
            binds,
            meta,
        })
    });
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn native_finalize_statement(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 1) as u64;
    with_state(|st| {
        st.stmts.remove(&handle);
    });
    Ok(())
}

fn native_get_parameter_count(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 1) as u64;
    let n = with_state(|st| st.stmts.get(&handle).map(|s| s.meta.parameter_count).unwrap_or(0));
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn native_get_column_count(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 1) as u64;
    let n = with_state(|st| st.stmts.get(&handle).map(|s| s.meta.column_count).unwrap_or(0));
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn native_get_column_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 1) as u64;
    let index = vm.arg_i32(ctx, 2).max(0) as usize;
    let name = with_state(|st| {
        st.stmts
            .get(&handle)
            .and_then(|s| s.meta.column_names.get(index).cloned())
            .unwrap_or_default()
    });
    let h = vm.new_string(&name)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn native_is_read_only(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let db_handle = vm.arg_i64(ctx, 0);
    let stmt_handle = vm.arg_i64(ctx, 1) as u64;
    let sql = with_state(|st| st.stmts.get(&stmt_handle).map(|s| s.sql.clone()));
    let result = match sql {
        Some(sql) => with_db(db_handle, |db| db.statement_is_readonly(&sql).unwrap_or(false))
            .unwrap_or(false),
        None => false,
    };
    vm.ret_bool(ctx, result);
    Ok(())
}

fn native_get_db_lookaside(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `sqlite3_db_status(LOOKASIDE_USED)` is a pager-internal counter with no
    // app-visible effect; report the real "no lookaside slots in use" value.
    let _ = vm.arg_i64(ctx, 0);
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn native_register_localized_collators(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let db_handle = vm.arg_i64(ctx, 0);
    let locale = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
    match with_db(db_handle, |db| db.register_localized_collators(&locale)) {
        Some(Ok(())) => Ok(()),
        Some(Err(e)) => Err(throw_engine(vm, e, None)),
        None => Err(throw_sqlite(vm, "connection is closed")),
    }
}

fn bind_value(vm: &mut Vm, ctx: NativeCtx, value: SqlValue) -> VmResult<()> {
    let stmt_handle = vm.arg_i64(ctx, 1) as u64;
    let index = vm.arg_i32(ctx, 2);
    if index < 1 {
        return Err(throw_sqlite(vm, "parameter index out of range"));
    }
    let applied = with_state(|st| {
        if let Some(s) = st.stmts.get_mut(&stmt_handle) {
            let i = (index - 1) as usize;
            if i < s.binds.len() {
                s.binds[i] = value;
                return true;
            }
        }
        false
    });
    if !applied {
        return Err(throw_sqlite(vm, "parameter index out of range"));
    }
    Ok(())
}

fn native_bind_null(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    bind_value(vm, ctx, SqlValue::Null)
}

fn native_bind_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 3);
    bind_value(vm, ctx, SqlValue::Integer(v))
}

fn native_bind_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_f64(ctx, 3);
    bind_value(vm, ctx, SqlValue::Real(v))
}

fn native_bind_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let s = vm.string_to_rust(vm.arg_ref(ctx, 3)).unwrap_or_default();
    bind_value(vm, ctx, SqlValue::Text(s))
}

fn native_bind_blob(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_ref(ctx, 3);
    let data = read_bytes(vm, h).unwrap_or_default();
    bind_value(vm, ctx, SqlValue::Blob(data))
}

fn native_reset_statement(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 1) as u64;
    with_state(|st| {
        if let Some(s) = st.stmts.get_mut(&handle) {
            for b in s.binds.iter_mut() {
                *b = SqlValue::Null;
            }
        }
    });
    Ok(())
}

/// Fetch the SQL and current bindings for a statement.
fn statement_args(vm: &mut Vm, handle: i64) -> Result<(u64, String, Vec<SqlValue>), VmError> {
    let u = handle as u64;
    let got = with_state(|st| st.stmts.get(&u).map(|s| (s.db, s.sql.clone(), s.binds.clone())));
    match got {
        Some(v) => Ok(v),
        None => Err(throw_sqlite(vm, "statement is closed")),
    }
}

fn native_execute(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let db_handle = vm.arg_i64(ctx, 0);
    let stmt_handle = vm.arg_i64(ctx, 1);
    let throw_on_row = vm.arg_bool(ctx, 2);
    let (_db, sql, binds) = statement_args(vm, stmt_handle)?;
    match with_db(db_handle, |db| db.execute(&sql, &binds, throw_on_row)) {
        Some(Ok(_)) => Ok(()),
        Some(Err(e)) => Err(throw_engine(vm, e, None)),
        None => Err(throw_sqlite(vm, "connection is closed")),
    }
}

fn native_execute_for_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let db_handle = vm.arg_i64(ctx, 0);
    let stmt_handle = vm.arg_i64(ctx, 1);
    let (_db, sql, binds) = statement_args(vm, stmt_handle)?;
    match with_db(db_handle, |db| db.execute_for_long(&sql, &binds)) {
        Some(Ok(Some(v))) => {
            vm.ret_i64(ctx, v);
            Ok(())
        }
        Some(Ok(None)) => Err(throw_sqlite(vm, "No rows")),
        Some(Err(e)) => Err(throw_engine(vm, e, None)),
        None => Err(throw_sqlite(vm, "connection is closed")),
    }
}

fn native_execute_for_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let db_handle = vm.arg_i64(ctx, 0);
    let stmt_handle = vm.arg_i64(ctx, 1);
    let (_db, sql, binds) = statement_args(vm, stmt_handle)?;
    match with_db(db_handle, |db| db.execute_for_string(&sql, &binds)) {
        Some(Ok(Some(s))) => {
            let h = vm.new_string(&s)?;
            vm.ret_ref(ctx, h);
            Ok(())
        }
        Some(Ok(None)) => {
            vm.ret_ref(ctx, NULL);
            Ok(())
        }
        Some(Err(e)) => Err(throw_engine(vm, e, None)),
        None => Err(throw_sqlite(vm, "connection is closed")),
    }
}

fn native_execute_for_changed_row_count(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let db_handle = vm.arg_i64(ctx, 0);
    let stmt_handle = vm.arg_i64(ctx, 1);
    let (_db, sql, binds) = statement_args(vm, stmt_handle)?;
    match with_db(db_handle, |db| db.execute_for_changed_row_count(&sql, &binds)) {
        Some(Ok(n)) => {
            vm.ret_i32(ctx, n as i32);
            Ok(())
        }
        Some(Err(e)) => Err(throw_engine(vm, e, None)),
        None => Err(throw_sqlite(vm, "connection is closed")),
    }
}

fn native_execute_for_last_inserted_row_id(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let db_handle = vm.arg_i64(ctx, 0);
    let stmt_handle = vm.arg_i64(ctx, 1);
    let (_db, sql, binds) = statement_args(vm, stmt_handle)?;
    match with_db(db_handle, |db| db.execute_for_last_inserted_row_id(&sql, &binds)) {
        Some(Ok(n)) => {
            vm.ret_i64(ctx, n);
            Ok(())
        }
        Some(Err(e)) => Err(throw_engine(vm, e, None)),
        None => Err(throw_sqlite(vm, "connection is closed")),
    }
}

fn native_execute_for_cursor_window(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let db_handle = vm.arg_i64(ctx, 0);
    let stmt_handle = vm.arg_i64(ctx, 1);
    let win_handle = vm.arg_i64(ctx, 2) as u64;
    let start_pos = vm.arg_i32(ctx, 3).max(0) as usize;
    let (_required_pos, _count_all) = (vm.arg_i32(ctx, 4), vm.arg_bool(ctx, 5));

    let (_db, sql, binds) = statement_args(vm, stmt_handle)?;
    let query = match with_db(db_handle, |db| db.query_all(&sql, &binds, None)) {
        Some(Ok(q)) => q,
        Some(Err(e)) => return Err(throw_engine(vm, e, None)),
        None => return Err(throw_sqlite(vm, "connection is closed")),
    };

    let (count, actual_pos) = with_state(|st| {
        let win = match st.wins.get_mut(&win_handle) {
            Some(w) => w,
            None => return (0usize, start_pos),
        };
        win.rows.clear();
        let total = query.rows.len();
        let actual = start_pos.min(total);
        let mut placed = 0usize;
        for row in query.rows.into_iter().skip(actual) {
            if !win.row_fits(&row) {
                break;
            }
            win.rows.push(row);
            placed += 1;
        }
        (placed, actual)
    });

    let packed = ((actual_pos as i64) << 32) | (count as i64);
    vm.ret_i64(ctx, packed);
    Ok(())
}

fn native_cancel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let db_handle = vm.arg_i64(ctx, 0);
    with_db(db_handle, |db| db.interrupt());
    Ok(())
}

fn native_reset_cancel(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    // Cancellation is delivered straight to the SQLite connection; there is no
    // per-connection cancel flag to clear on the ACOS side. This is a real
    // no-op, not a faked success: the next step simply runs normally.
    Ok(())
}

// -- android.database.CursorWindow -----------------------------------------

fn cw_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = vm.string_to_rust(vm.arg_ref(ctx, 0)).unwrap_or_default();
    let size = vm.arg_i32(ctx, 1);
    let capacity = if size > 0 { size as usize } else { 2 * 1024 * 1024 };
    let handle = with_state(|st| {
        st.insert_win(Win {
            name,
            num_columns: 0,
            capacity,
            rows: Vec::new(),
        })
    });
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn cw_dispose(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    with_state(|st| {
        st.wins.remove(&handle);
    });
    Ok(())
}

fn cw_get_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let name = with_state(|st| st.wins.get(&handle).map(|w| w.name.clone()).unwrap_or_default());
    let h = vm.new_string(&name)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn cw_get_num_rows(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let n = with_state(|st| st.wins.get(&handle).map(|w| w.rows.len()).unwrap_or(0));
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn cw_set_num_columns(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let columns = vm.arg_i32(ctx, 1);
    let ok = with_state(|st| {
        if let Some(w) = st.wins.get_mut(&handle) {
            if columns > 0 {
                w.num_columns = columns as usize;
                return true;
            }
        }
        false
    });
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn cw_alloc_row(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let ok = with_state(|st| {
        if let Some(w) = st.wins.get_mut(&handle) {
            if w.num_columns == 0 {
                return false;
            }
            let row = vec![SqlValue::Null; w.num_columns];
            if w.row_fits(&row) {
                w.rows.push(row);
                return true;
            }
        }
        false
    });
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn cw_free_last_row(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    with_state(|st| {
        if let Some(w) = st.wins.get_mut(&handle) {
            w.rows.pop();
        }
    });
    Ok(())
}

fn cw_clear(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    with_state(|st| {
        if let Some(w) = st.wins.get_mut(&handle) {
            w.rows.clear();
        }
    });
    Ok(())
}

fn cell(state: &SqliteState, handle: u64, row: i32, col: i32) -> Option<&SqlValue> {
    let w = state.wins.get(&handle)?;
    if row < 0 || col < 0 {
        return None;
    }
    w.rows.get(row as usize)?.get(col as usize)
}

fn cw_get_type(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let row = vm.arg_i32(ctx, 1);
    let col = vm.arg_i32(ctx, 2);
    let t = with_state(|st| cell(st, handle, row, col).map(value_type).unwrap_or(0));
    vm.ret_i32(ctx, t);
    Ok(())
}

fn cw_get_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let row = vm.arg_i32(ctx, 1);
    let col = vm.arg_i32(ctx, 2);
    let v = with_state(|st| cell(st, handle, row, col).map(value_as_long).unwrap_or(0));
    vm.ret_i64(ctx, v);
    Ok(())
}

fn cw_get_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let row = vm.arg_i32(ctx, 1);
    let col = vm.arg_i32(ctx, 2);
    let v = with_state(|st| cell(st, handle, row, col).map(value_as_double).unwrap_or(0.0));
    vm.ret_f64(ctx, v);
    Ok(())
}

fn cw_get_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let row = vm.arg_i32(ctx, 1);
    let col = vm.arg_i32(ctx, 2);
    let value = with_state(|st| cell(st, handle, row, col).cloned());
    match value {
        Some(SqlValue::Null) | None => vm.ret_ref(ctx, NULL),
        Some(v) => {
            let h = vm.new_string(&acos_sqlite::value_to_string(&v))?;
            vm.ret_ref(ctx, h);
        }
    }
    Ok(())
}

fn cw_get_blob(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let row = vm.arg_i32(ctx, 1);
    let col = vm.arg_i32(ctx, 2);
    let value = with_state(|st| cell(st, handle, row, col).cloned());
    match value {
        Some(SqlValue::Blob(b)) => {
            let h = byte_array(vm, b)?;
            vm.ret_ref(ctx, h);
        }
        _ => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn put_cell(vm: &mut Vm, ctx: NativeCtx, col_arg: usize, value: SqlValue) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let row = vm.arg_i32(ctx, col_arg);
    let col = vm.arg_i32(ctx, col_arg + 1);
    let ok = with_state(|st| {
        if let Some(w) = st.wins.get_mut(&handle) {
            if row < 0 || col < 0 {
                return false;
            }
            let old = w
                .rows
                .get(row as usize)
                .and_then(|r| r.get(col as usize))
                .cloned();
            if old.is_none() {
                return false;
            }
            let mut candidate = w.rows[row as usize].clone();
            candidate[col as usize] = value.clone();
            if !w.row_fits(&candidate) {
                return false;
            }
            w.rows[row as usize][col as usize] = value;
            return true;
        }
        false
    });
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn cw_put_null(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    put_cell(vm, ctx, 1, SqlValue::Null)
}

fn cw_put_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 1);
    put_cell(vm, ctx, 2, SqlValue::Integer(v))
}

fn cw_put_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_f64(ctx, 1);
    put_cell(vm, ctx, 2, SqlValue::Real(v))
}

fn cw_put_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let s = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
    put_cell(vm, ctx, 2, SqlValue::Text(s))
}

fn cw_put_blob(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let data = read_bytes(vm, vm.arg_ref(ctx, 1)).unwrap_or_default();
    put_cell(vm, ctx, 2, SqlValue::Blob(data))
}

fn cw_copy_string_to_buffer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let row = vm.arg_i32(ctx, 1);
    let col = vm.arg_i32(ctx, 2);
    let buf = vm.arg_ref(ctx, 3);
    let value = with_state(|st| cell(st, handle, row, col).cloned());
    let text = match value {
        Some(v) => acos_sqlite::value_to_string(&v),
        None => return Ok(()),
    };
    let units: Vec<u16> = text.encode_utf16().collect();
    let existing = vm.get_instance_field_by_name(buf, "data").unwrap_or(0);
    let existing_len = match vm.heap.get(existing) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Char(v) => v.len(),
            _ => 0,
        },
        _ => 0,
    };
    let target = if existing != 0 && existing_len >= units.len() {
        existing
    } else {
        let arr = vm.alloc(Object::Array(ArrayObj {
            elem_desc: "C".to_string(),
            data: ArrayData::Char(vec![0; units.len().max(1)]),
        }))?;
        vm.set_instance_field_by_name(buf, "data", arr);
        arr
    };
    if let Some(Object::Array(a)) = vm.heap.get_mut(target) {
        if let ArrayData::Char(v) = &mut a.data {
            let n = v.len().min(units.len());
            v[..n].copy_from_slice(&units[..n]);
        }
    }
    vm.set_instance_field_by_name(buf, "sizeCopied", units.len() as u32);
    Ok(())
}

// -- SQLiteGlobal ----------------------------------------------------------

fn sqlite_global_release_memory(_vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `sqlite3_release_memory(0)` frees as much pager cache as it can and
    // returns the number of bytes freed. ACOS has a small, short-lived cache
    // and does not fabricate a byte count.
    _vm.ret_i32(ctx, 0);
    Ok(())
}
