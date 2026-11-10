import android.content.ContentValues;
import android.database.Cursor;
import android.database.sqlite.SQLiteConstraintException;
import android.database.sqlite.SQLiteDatabase;

import java.io.File;

/**
 * Android SQLite CRUD fixture (dexprog 28).
 *
 * Exercises the framework path `SQLiteDatabase` -> `SQLiteConnectionPool` ->
 * `SQLiteSession` -> `SQLiteConnection.native*` and `CursorWindow.native*`:
 * create / insert / query / update / delete, transactions + rollback,
 * UNIQUE and NOT NULL constraint errors, BLOB round-trip, NULL handling and
 * the built-in BINARY vs NOCASE collation.
 *
 * The program is deterministic: it deletes its database first, prints no
 * paths, and relies on `AUTOINCREMENT` only after a clean create.
 *
 * Two writable directories exist in the two environments this runs in:
 *   * ACOS: the VFS sandbox maps /data/user/0/<pkg> into the session root.
 *   * ART (adb shell / dalvikvm): /data/local/tmp is writable, /data/user/0
 *     is not, so it falls back there.
 * The chosen path is never printed, so stdout stays identical.
 */
public class Main {
    private static final String DB_NAME = "acos28.db";
    private static final char[] HEX = "0123456789abcdef".toCharArray();

    private static File databaseDir() {
        File app = new File("/data/user/0/dev.acos.fixtures.sqlite28/databases");
        if (app.mkdirs() || app.isDirectory()) {
            return app;
        }
        File fallback = new File("/data/local/tmp/acos-sqlite-28");
        fallback.mkdirs();
        return fallback;
    }

    private static void deleteDatabase(File dir) {
        new File(dir, DB_NAME).delete();
        new File(dir, DB_NAME + "-journal").delete();
        new File(dir, DB_NAME + "-wal").delete();
        new File(dir, DB_NAME + "-shm").delete();
    }

    private static String hex(byte[] data) {
        StringBuilder sb = new StringBuilder(data.length * 2);
        for (byte b : data) {
            int v = b & 0xff;
            sb.append(HEX[v >>> 4]);
            sb.append(HEX[v & 0xf]);
        }
        return sb.toString();
    }

    private static long scalar(SQLiteDatabase db, String sql) {
        Cursor c = db.rawQuery(sql, null);
        try {
            return c.moveToFirst() ? c.getLong(0) : -1L;
        } finally {
            c.close();
        }
    }

    private static String column(SQLiteDatabase db, String sql, String separator) {
        Cursor c = db.rawQuery(sql, null);
        StringBuilder sb = new StringBuilder();
        try {
            while (c.moveToNext()) {
                if (sb.length() > 0) {
                    sb.append(separator);
                }
                sb.append(c.getString(0));
            }
        } finally {
            c.close();
        }
        return sb.toString();
    }

    private static long insertOrThrow(SQLiteDatabase db, String body) {
        ContentValues values = new ContentValues();
        values.put("body", body);
        return db.insertOrThrow("notes", null, values);
    }

    public static void main(String[] args) {
        File dir = databaseDir();
        deleteDatabase(dir);
        File dbFile = new File(dir, DB_NAME);

        SQLiteDatabase db = SQLiteDatabase.openOrCreateDatabase(dbFile, null);
        System.out.println("opened=ok");

        db.execSQL("CREATE TABLE notes ("
                + "_id INTEGER PRIMARY KEY AUTOINCREMENT, "
                + "body TEXT NOT NULL UNIQUE)");
        System.out.println("create=ok");

        long id1 = insertOrThrow(db, "alpha");
        long id2 = insertOrThrow(db, "beta");
        long id3 = insertOrThrow(db, "gamma");
        System.out.println("insert_ids=" + id1 + "," + id2 + "," + id3);
        System.out.println("count=" + scalar(db, "SELECT COUNT(*) FROM notes"));

        System.out.println("rows=" + column(db, "SELECT _id || ':' || body FROM notes ORDER BY _id ASC", "|"));

        ContentValues update = new ContentValues();
        update.put("body", "alpha-1");
        int updated = db.update("notes", update, "_id = ?", new String[] {String.valueOf(id1)});
        System.out.println("updated=" + updated);
        System.out.println("after_update=" + column(db, "SELECT body FROM notes WHERE _id = " + id1, ","));

        int deleted = db.delete("notes", "_id = ?", new String[] {String.valueOf(id2)});
        System.out.println("deleted=" + deleted);
        System.out.println("after_delete=" + scalar(db, "SELECT COUNT(*) FROM notes"));

        Cursor all = db.query("notes", new String[] {"_id"},
                null, null, null, null, "_id ASC");
        try {
            System.out.println("cursor_count=" + all.getCount());
            System.out.println("first_id=" + (all.moveToFirst() ? all.getLong(0) : -1L));
            System.out.println("last_id=" + (all.moveToLast() ? all.getLong(0) : -1L));
        } finally {
            all.close();
        }

        // Transactions: commit two rows, then roll one back.
        db.beginTransaction();
        try {
            insertOrThrow(db, "tx0");
            insertOrThrow(db, "tx1");
            db.setTransactionSuccessful();
        } finally {
            db.endTransaction();
        }
        System.out.println("tx_count=" + scalar(db, "SELECT COUNT(*) FROM notes"));

        db.beginTransaction();
        try {
            insertOrThrow(db, "rollback");
        } finally {
            db.endTransaction();
        }
        System.out.println("rollback_count=" + scalar(db, "SELECT COUNT(*) FROM notes"));

        // UNIQUE constraint.
        try {
            insertOrThrow(db, "alpha-1");
            System.out.println("unique=none");
        } catch (SQLiteConstraintException e) {
            System.out.println("unique=" + e.getClass().getSimpleName() + ": " + e.getMessage());
        }

        // NOT NULL constraint.
        try {
            ContentValues nullBody = new ContentValues();
            nullBody.putNull("body");
            db.insertOrThrow("notes", null, nullBody);
            System.out.println("notnull=none");
        } catch (SQLiteConstraintException e) {
            System.out.println("notnull=" + e.getClass().getSimpleName() + ": " + e.getMessage());
        }

        // BLOB round-trip and CursorWindow type.
        db.execSQL("CREATE TABLE blobs (_id INTEGER PRIMARY KEY, data BLOB)");
        byte[] payload = new byte[] {0x00, (byte) 0xff, 0x10, (byte) 0xab};
        ContentValues blob = new ContentValues();
        blob.put("_id", 1);
        blob.put("data", payload);
        db.insertOrThrow("blobs", null, blob);
        Cursor bc = db.query("blobs", new String[] {"data"},
                "_id = ?", new String[] {"1"}, null, null, null);
        try {
            bc.moveToFirst();
            System.out.println("blob_type=" + bc.getType(0));
            System.out.println("blob_len=" + bc.getBlob(0).length);
            System.out.println("blob_hex=" + hex(bc.getBlob(0)));
        } finally {
            bc.close();
        }

        // Value types and NULL.
        db.execSQL("CREATE TABLE types ("
                + "_id INTEGER PRIMARY KEY, i INTEGER, r REAL, t TEXT, b BLOB, n TEXT)");
        ContentValues types = new ContentValues();
        types.put("_id", 1);
        types.put("i", 42L);
        types.put("r", 3.5d);
        types.put("t", "text");
        types.put("b", new byte[] {1, 2, 3});
        db.insertOrThrow("types", null, types);
        Cursor tc = db.query("types", new String[] {"i", "r", "t", "b", "n"},
                null, null, null, null, null);
        try {
            tc.moveToFirst();
            System.out.println("types=" + tc.getType(0) + "," + tc.getType(1) + ","
                    + tc.getType(2) + "," + tc.getType(3) + "," + tc.getType(4));
            System.out.println("type_values=" + tc.getLong(0) + "," + tc.getDouble(1)
                    + "," + tc.getString(2) + "," + tc.getBlob(3).length + ","
                    + tc.isNull(4) + "," + tc.getString(4));
        } finally {
            tc.close();
        }

        // Built-in collation: BINARY vs NOCASE.
        db.execSQL("CREATE TABLE words (word TEXT)");
        db.execSQL("INSERT INTO words (word) VALUES (?)", new Object[] {"Beta"});
        db.execSQL("INSERT INTO words (word) VALUES (?)", new Object[] {"alpha"});
        db.execSQL("INSERT INTO words (word) VALUES (?)", new Object[] {"Gamma"});
        System.out.println("binary=" + column(db,
                "SELECT word FROM words ORDER BY word", ","));
        System.out.println("nocase=" + column(db,
                "SELECT word FROM words ORDER BY word COLLATE NOCASE", ","));

        db.close();
        System.out.println("closed=ok");
        System.out.println("done");
    }
}
