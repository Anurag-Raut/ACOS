package dev.acos.fixtures.svcprovider;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.UriMatcher;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.util.Log;

import java.util.ArrayList;
import java.util.List;

/**
 * In-memory {@link ContentProvider} for the service/provider/receiver round
 * trip. Plain framework APIs only.
 *
 * The provider is deliberately non-persistent: the point of this fixture is the
 * Binder/AMS/ContentResolver plumbing (insert + query across processes), not
 * SQLite (that is the sqlite-crud fixture).
 */
public class EchoProvider extends ContentProvider {
    public static final String AUTHORITY = "dev.acos.fixtures.svcprovider";
    public static final Uri CONTENT_URI = Uri.parse("content://" + AUTHORITY + "/echo");

    private static final String TAG = "AcosSvc";
    private static final int ALL = 1;
    private static final int ONE = 2;

    private static final UriMatcher MATCHER = new UriMatcher(UriMatcher.NO_MATCH);
    static {
        MATCHER.addURI(AUTHORITY, "echo", ALL);
        MATCHER.addURI(AUTHORITY, "echo/#", ONE);
    }

    private final List<ContentValues> rows = new ArrayList<ContentValues>();

    @Override
    public boolean onCreate() {
        Log.i(TAG, "provider.onCreate authority=" + AUTHORITY);
        return true;
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection,
                        String[] selectionArgs, String sortOrder) {
        Log.i(TAG, "provider.query uri=" + uri);
        MatrixCursor cursor = new MatrixCursor(new String[] {"_id", "payload"});
        synchronized (rows) {
            int match = MATCHER.match(uri);
            for (int i = 0; i < rows.size(); i++) {
                if (match == ONE && !String.valueOf(i).equals(uri.getLastPathSegment())) {
                    continue;
                }
                cursor.addRow(new Object[] {i, rows.get(i).getAsString("payload")});
            }
        }
        return cursor;
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        long id;
        synchronized (rows) {
            id = rows.size();
            rows.add(new ContentValues(values));
        }
        Log.i(TAG, "provider.insert id=" + id + " payload=" + values.getAsString("payload"));
        if (getContext() != null) {
            getContext().getContentResolver().notifyChange(uri, null);
        }
        return Uri.withAppendedPath(CONTENT_URI, String.valueOf(id));
    }

    @Override
    public int delete(Uri uri, String selection, String[] selectionArgs) {
        int before;
        synchronized (rows) {
            before = rows.size();
            rows.clear();
        }
        Log.i(TAG, "provider.delete removed=" + before);
        return before;
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] selectionArgs) {
        Log.i(TAG, "provider.update uri=" + uri);
        return 0;
    }

    @Override
    public String getType(Uri uri) {
        return "vnd.android.cursor.dir/vnd.acos.echo";
    }
}
