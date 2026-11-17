package dev.acos.fixtures.procb;

import android.app.Service;
import android.content.Intent;
import android.os.Binder;
import android.os.IBinder;
import android.os.Parcel;
import android.util.Log;

/**
 * MPROC4 step 3c: a binder object that **lives in the second process**.
 *
 * <p>proc-a binds this service with `bindService`; the binder it receives is a
 * handle in *its* process that addresses an object in *this* one. The round trip
 * is a plain `IBinder.transact` (the same entry point every AIDL stub's
 * `onTransact` is reached through; this fixture writes the parcel by hand so it
 * needs no `aidl` tool in the fixture build):
 *
 * <pre>
 * ACOSPROBE b transact code=1 in=ping -> out=echo:ping
 * </pre>
 *
 * <p>The service's own process identity is logged with every transaction, so a
 * reply that arrived from another process cannot be mistaken for a local one.
 */
public class EchoService extends Service {
    private static final String TAG = "AcosProcB";

    /** Transaction code of the echo call. */
    public static final int TRANSACTION_ECHO = IBinder.FIRST_CALL_TRANSACTION;

    private final IBinder binder = new Binder() {
        @Override
        protected boolean onTransact(int code, Parcel data, Parcel reply, int flags) {
            if (code == TRANSACTION_ECHO) {
                data.enforceInterface("dev.acos.fixtures.procb.IEcho");
                String in = data.readString();
                String out = "echo:" + in;
                if (reply != null) {
                    reply.writeNoException();
                    reply.writeString(out);
                }
                Log.i(TAG, "ACOSPROBE b transact code=" + code
                        + " in=" + in + " -> out=" + out
                        + " pid=" + android.os.Process.myPid());
                return true;
            }
            try {
                return super.onTransact(code, data, reply, flags);
            } catch (android.os.RemoteException e) {
                throw new RuntimeException(e);
            }
        }
    };

    @Override
    public void onCreate() {
        super.onCreate();
        Log.i(TAG, "onCreate service pid=" + android.os.Process.myPid());
    }

    @Override
    public IBinder onBind(Intent intent) {
        Log.i(TAG, "ACOSPROBE b onBind pid=" + android.os.Process.myPid());
        return binder;
    }

    @Override
    public boolean onUnbind(Intent intent) {
        Log.i(TAG, "ACOSPROBE b onUnbind");
        return super.onUnbind(intent);
    }
}
