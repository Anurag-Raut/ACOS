//! Engine-level tests for the ACOS SQLite facade.
//!
//! These prove the semantics the `android.database.sqlite` natives rely on:
//! CRUD, transactions/rollback, BLOB + NULL round-trips, `last_insert_rowid`,
//! and the exact primary/extended result codes Android maps to exception
//! subclasses.

use acos_sqlite::{Database, SqlValue};

fn text(s: &str) -> SqlValue {
    SqlValue::Text(s.to_string())
}

#[test]
fn crud_transactions_and_constraints() {
    let db = Database::open(":memory:", false, true).expect("open");
    db.execute(
        "CREATE TABLE notes (_id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL UNIQUE)",
        &[],
        false,
    )
    .expect("create");

    let mut last_id = 0;
    for body in ["alpha", "beta", "gamma"] {
        last_id = db
            .execute_for_last_inserted_row_id("INSERT INTO notes (body) VALUES (?)", &[text(body)])
            .expect("insert");
    }
    assert_eq!(last_id, 3);
    assert_eq!(
        db.query_all("SELECT COUNT(*) FROM notes", &[], None).unwrap().rows[0][0],
        SqlValue::Integer(3)
    );

    // update / delete
    assert_eq!(db.execute("UPDATE notes SET body = 'alpha-1' WHERE body = 'alpha'", &[], false).unwrap(), 1);
    assert_eq!(db.execute("DELETE FROM notes WHERE body = 'beta'", &[], false).unwrap(), 1);

    // transaction commit
    db.execute("BEGIN EXCLUSIVE;", &[], false).unwrap();
    db.execute("INSERT INTO notes (body) VALUES ('tx')", &[], false).unwrap();
    db.execute("COMMIT;", &[], false).unwrap();
    let count = |db: &Database| match db.query_all("SELECT COUNT(*) FROM notes", &[], None).unwrap().rows[0][0].clone() {
        SqlValue::Integer(i) => i,
        _ => -1,
    };
    let after_tx = count(&db);

    // transaction rollback
    db.execute("BEGIN EXCLUSIVE;", &[], false).unwrap();
    db.execute("INSERT INTO notes (body) VALUES ('rollback')", &[], false).unwrap();
    db.execute("ROLLBACK;", &[], false).unwrap();
    assert_eq!(count(&db), after_tx);

    // constraint error codes
    let unique = db.execute("INSERT INTO notes (body) VALUES ('alpha-1')", &[], false).unwrap_err();
    assert_eq!(unique.code, 19);
    assert_eq!(unique.extended, 2067);
    assert_eq!(unique.message, "UNIQUE constraint failed: notes.body");

    let notnull = db.execute("INSERT INTO notes (body) VALUES (NULL)", &[], false).unwrap_err();
    assert_eq!(notnull.code, 19);
    assert_eq!(notnull.extended, 1299);
    assert_eq!(notnull.message, "NOT NULL constraint failed: notes.body");
}

#[test]
fn blob_null_and_types() {
    let db = Database::open(":memory:", false, true).unwrap();
    db.execute("CREATE TABLE t (_id INTEGER PRIMARY KEY, i INTEGER, r REAL, t TEXT, b BLOB, n TEXT)", &[], false)
        .unwrap();
    db.execute(
        "INSERT INTO t (_id, i, r, t, b) VALUES (1, 42, 3.5, 'text', ?)",
        &[SqlValue::Blob(vec![0x00, 0xff, 0x10, 0xab])],
        false,
    )
    .unwrap();
    let q = db.query_all("SELECT i, r, t, b, n FROM t", &[], None).unwrap();
    assert_eq!(q.rows.len(), 1);
    assert_eq!(q.rows[0][0], SqlValue::Integer(42));
    assert_eq!(q.rows[0][1], SqlValue::Real(3.5));
    assert_eq!(q.rows[0][2], text("text"));
    assert_eq!(q.rows[0][3], SqlValue::Blob(vec![0x00, 0xff, 0x10, 0xab]));
    assert_eq!(q.rows[0][4], SqlValue::Null);
    assert_eq!(q.columns, vec!["i", "r", "t", "b", "n"]);
}

#[test]
fn prepare_errors_carry_primary_code() {
    let db = Database::open(":memory:", false, true).unwrap();
    let e = db.query_all("SELECT * FROM nope", &[], None).unwrap_err();
    assert_eq!(e.code, 1);
    assert_eq!(e.extended, 1);
    assert_eq!(e.message, "no such table: nope");
}
