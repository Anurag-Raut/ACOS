package dev.acos.fixtures.proca;

import android.app.Activity;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.ServiceConnection;
import android.os.Binder;
import android.os.Bundle;
import android.os.IBinder;
import android.os.Parcel;
import android.os.Process;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

/**
 * MPROC4 step 3b/3c: the app in the **first** process.
 *
 * <p>Run as `acos-art launch --apk proc-a.apk --extra-apk proc-b.apk`. Every
 * action is a real framework call to a component of the other installed package,
 * so the host has to start a second process for it:
 *
 * <ul>
 *   <li>{@code Start B} — `startActivityForResult` to
 *       `dev.acos.fixtures.procb/.ProcBActivity`. Lazy spawn: B's process exists
 *       only because of this call.</li>
 *   <li>{@code Bind B} — `bindService` to `dev.acos.fixtures.procb/.EchoService`
 *       and one `IBinder.transact` round trip through the foreign object.</li>
 *   <li>{@code Unbind B} — releases the connection (the holder release).</li>
 * </ul>
 *
 * <p>Lifecycle and result facts are printed as one `ACOSPROBE` line each, so the
 * launch reply, the log and the tree all carry the same evidence:
 *
 * <pre>
 * ACOSPROBE a ready pkg=dev.acos.fixtures.proca
 * ACOSPROBE a start-b result=0
 * ACOSPROBE a paused
 * ACOSPROBE a result request=7 code=-1 data=from-b
 * ACOSPROBE a bind-b ok
 * ACOSPROBE a transact out=echo:ping
 * </pre>
 */
public class ProcAActivity extends Activity {
    private static final String TAG = "AcosProcA";

    /** The package that must be spawned on demand. */
    public static final String PKG_B = "dev.acos.fixtures.procb";
    /** Request code carried through `startActivityForResult`. */
    public static final int REQUEST_B = 7;
    /** The note the launch intent carries (B logs it back). */
    public static final String EXTRA_NOTE = "note";
    /** Transaction code of B's echo call. */
    public static final int TRANSACTION_ECHO = IBinder.FIRST_CALL_TRANSACTION;

    private TextView status;
    private IBinder service;
    private boolean bound;

    private final ServiceConnection connection = new ServiceConnection() {
        @Override
        public void onServiceConnected(ComponentName name, IBinder binder) {
            service = binder;
            bound = true;
            Log.i(TAG, "ACOSPROBE a bind-b ok component=" + name.flattenToShortString());
            transact();
        }

        @Override
        public void onServiceDisconnected(ComponentName name) {
            service = null;
            bound = false;
            Log.i(TAG, "ACOSPROBE a service-disconnected component="
                    + name.flattenToShortString());
        }

        @Override
        public void onBindingDied(ComponentName name) {
            Log.i(TAG, "ACOSPROBE a binding-died component=" + name.flattenToShortString());
        }

        @Override
        public void onNullBinding(ComponentName name) {
            Log.i(TAG, "ACOSPROBE a null-binding component=" + name.flattenToShortString());
        }
    };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        status = (TextView) findViewById(R.id.status);
        ((Button) findViewById(R.id.start_b)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startB();
            }
        });
        ((Button) findViewById(R.id.bind_b)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                bindB();
            }
        });
        ((Button) findViewById(R.id.unbind_b)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                unbindB();
            }
        });
        Log.i(TAG, "ACOSPROBE a ready pkg=" + getPackageName()
                + " pid=" + android.os.Process.myPid());
        probeIdentity();
        status.setText("A ready");
    }

    /**
     * BINDERPID: what this process's own binder identity is, and what the host
     * answers for it.
     *
     * <p>On a device a process's own thread answers `Binder.getCallingUid()` with
     * its own uid (`IPCThreadState`'s calling identity outside a transaction) and
     * `Context.checkCallingPermission` asks the permission service for it by uid.
     * The two apps in this session have different uids, so the permission answer
     * must differ per app: {@code granted_to} is only one of them.
     */
    private void probeIdentity() {
        int uid = Process.myUid();
        int pid = Process.myPid();
        int callingUid = Binder.getCallingUid();
        int callingPid = Binder.getCallingPid();
        String owner = getPackageManager().getNameForUid(uid);
        int perm = checkCallingPermission("android.permission.INTERNET");
        Log.i(TAG, "ACOSPROBE a identity pkg=" + getPackageName()
                + " uid=" + uid + " pid=" + pid
                + " calling-uid=" + callingUid + " calling-pid=" + callingPid
                + " uid-pkg=" + owner
                + " internet=" + perm);
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "ACOSPROBE a resumed");
    }

    @Override
    protected void onPause() {
        super.onPause();
        Log.i(TAG, "ACOSPROBE a paused");
    }

    @Override
    protected void onStop() {
        super.onStop();
        Log.i(TAG, "ACOSPROBE a stopped");
    }

    /** `startActivityForResult` for B's activity: the lazy-spawn trigger. */
    private void startB() {
        Intent intent = new Intent();
        intent.setComponent(new ComponentName(PKG_B, PKG_B + ".ProcBActivity"));
        intent.putExtra(EXTRA_NOTE, "hello");
        Log.i(TAG, "ACOSPROBE a start-b component=" + intent.getComponent().flattenToShortString());
        status.setText("A started B");
        startActivityForResult(intent, REQUEST_B);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        String value = data == null ? "" : data.getStringExtra("data");
        Log.i(TAG, "ACOSPROBE a result request=" + requestCode
                + " code=" + resultCode + " data=" + (value == null ? "none" : value));
        status.setText("A result " + resultCode + " " + value);
    }

    /** `bindService` to B's service: a binder object that lives in B. */
    private void bindB() {
        Intent intent = new Intent();
        intent.setComponent(new ComponentName(PKG_B, PKG_B + ".EchoService"));
        boolean ok = bindService(intent, connection, Context.BIND_AUTO_CREATE);
        Log.i(TAG, "ACOSPROBE a bind-b requested ok=" + ok);
        status.setText("A binding B");
        if (!ok) {
            Log.i(TAG, "ACOSPROBE a bind-b failed");
        }
    }

    /** One `transact` round trip through the foreign binder. */
    private void transact() {
        IBinder target = service;
        if (target == null) {
            Log.i(TAG, "ACOSPROBE a transact skipped (no binder)");
            return;
        }
        Parcel data = Parcel.obtain();
        Parcel reply = Parcel.obtain();
        try {
            data.writeInterfaceToken("dev.acos.fixtures.procb.IEcho");
            data.writeString("ping");
            boolean ok = target.transact(TRANSACTION_ECHO, data, reply, 0);
            reply.readException();
            String out = reply.readString();
            Log.i(TAG, "ACOSPROBE a transact ok=" + ok + " out=" + out
                    + " pid=" + android.os.Process.myPid());
            status.setText("A transact " + out);
        } catch (Throwable t) {
            Log.i(TAG, "ACOSPROBE a transact threw " + t);
            status.setText("A transact threw " + t);
        } finally {
            reply.recycle();
            data.recycle();
        }
    }

    /** Release the holder (the peer frees the synthetic node with it). */
    private void unbindB() {
        if (!bound) {
            Log.i(TAG, "ACOSPROBE a unbind-b skipped (not bound)");
            return;
        }
        try {
            unbindService(connection);
            bound = false;
            service = null;
            Log.i(TAG, "ACOSPROBE a unbind-b done");
            status.setText("A unbound B");
        } catch (Throwable t) {
            Log.i(TAG, "ACOSPROBE a unbind-b threw " + t);
        }
    }

    @Override
    protected void onDestroy() {
        if (bound) {
            try {
                unbindService(connection);
            } catch (Throwable ignored) {
            }
        }
        super.onDestroy();
        Log.i(TAG, "onDestroy");
    }
}
