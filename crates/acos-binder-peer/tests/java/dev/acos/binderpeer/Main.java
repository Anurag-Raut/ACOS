package dev.acos.binderpeer;

import android.os.Binder;
import android.os.IBinder;
import android.os.Parcel;

import java.lang.reflect.Method;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

import dev.acos.e3g.Nat;

/**
 * P2d acceptance: stock libbinder + the userspace driver + the REAL ACOS
 * ServiceHost peer (crates/acos-binder-peer), no /dev/binder, no system_server.
 *
 *   getService("window"/"activity_task"/"package") -> real proxies
 *   IWindowManager.hasNavigationBar(0)             -> real WmsService answer
 *   IActivityManager.checkPermission(...)          -> real ServiceHost answer
 *   unknown transaction code                       -> structured error
 *   20 peer-initiated IResultReceiver.send() calls -> no race failure
 */
public final class Main {
    static final int CODE_HAS_NAV_BAR = 73;
    static final int CODE_CHECK_PERMISSION = 9;
    static final String TOKEN_WM = "android.view.IWindowManager";
    static final String TOKEN_AM = "android.app.IActivityManager";
    static final String TOKEN_AUTOFILL = "android.view.autofill.IAutoFillManager";
    static final String TOKEN_RECEIVER = "com.android.internal.os.IResultReceiver";
    static final int CODE_AUTOFILL_ADD_CLIENT = 1;

    static int failures = 0;

    public static void main(String[] args) {
        load();
        stepServices();
        stepNavigationBar();
        stepServiceHostTransaction();
        stepUnknownCode();
        stepReverseDeliveries(20);
        System.out.println("RESULT failures=" + failures);
        System.out.flush();
        System.exit(failures == 0 ? 0 : 1);
    }

    static void load() {
        try {
            Nat.load();
        } catch (Throwable t) {
            System.out.println("OBSERVED driver already preloaded: " + t);
        }
        String[] names = {
                "register_android_util_Log",
                // `EventLog.writeEvent` is reached from the first framework IPC
                // in a cold dalvik-cache process; without it `getService()`
                // throws InvocationTargetException (UnsatisfiedLinkError).
                "register_android_util_EventLog",
                "register_android_os_SystemProperties",
                "register_android_os_SystemClock",
                "register_android_os_Trace",
                "register_android_os_Parcel",
                "register_android_os_Binder",
        };
        for (String n : names) {
            int rc = Nat.register(n);
            if (rc != 1) {
                fail("substrate " + n + " rc=" + rc);
                return;
            }
        }
        System.out.println("PASS substrate: 7 framework natives registered");
    }

    static IBinder service(String name) throws Exception {
        Class<?> sm = Class.forName("android.os.ServiceManager");
        Method m = sm.getDeclaredMethod("getService", String.class);
        m.setAccessible(true);
        return (IBinder) m.invoke(null, name);
    }

    static void stepServices() {
        for (String name : new String[] {"window", "activity_task", "package"}) {
            try {
                IBinder b = service(name);
                if (b == null) {
                    fail("getService(\"" + name + "\") -> null");
                } else {
                    System.out.println("PASS getService(\"" + name + "\") -> "
                            + b.getClass().getName() + " alive=" + b.isBinderAlive());
                }
            } catch (Throwable t) {
                fail("getService(\"" + name + "\") threw " + t);
            }
        }
        try {
            IBinder b = service("definitely.not.a.service");
            if (b == null) {
                System.out.println("PASS getService(unknown) -> null");
            } else {
                fail("getService(unknown) -> " + b);
            }
        } catch (Throwable t) {
            fail("getService(unknown) threw " + t);
        }
    }

    static void stepNavigationBar() {
        try {
            IBinder wm = service("window");
            Parcel data = Parcel.obtain();
            Parcel reply = Parcel.obtain();
            data.writeInterfaceToken(TOKEN_WM);
            data.writeInt(0);
            boolean ok = wm.transact(CODE_HAS_NAV_BAR, data, reply, 0);
            reply.readException();
            boolean has = reply.readInt() != 0;
            data.recycle();
            reply.recycle();
            if (ok && has) {
                System.out.println("PASS IWindowManager.hasNavigationBar(0) -> true (real WmsService)");
            } else {
                fail("hasNavigationBar: transact=" + ok + " value=" + has);
            }
        } catch (Throwable t) {
            fail("hasNavigationBar threw " + t);
        }
    }

    static void stepServiceHostTransaction() {
        try {
            IBinder am = service("activity");
            Parcel data = Parcel.obtain();
            Parcel reply = Parcel.obtain();
            data.writeInterfaceToken(TOKEN_AM);
            data.writeString("android.permission.INTERNET");
            data.writeInt(1000);
            data.writeInt(0);
            boolean ok = am.transact(CODE_CHECK_PERMISSION, data, reply, 0);
            reply.readException();
            int granted = reply.readInt();
            data.recycle();
            reply.recycle();
            if (ok && granted == 0) {
                System.out.println("PASS IActivityManager.checkPermission(INTERNET) -> PERMISSION_GRANTED");
            } else {
                fail("checkPermission: transact=" + ok + " result=" + granted);
            }
        } catch (Throwable t) {
            fail("checkPermission threw " + t);
        }
    }

    static void stepUnknownCode() {
        try {
            IBinder am = service("activity");
            Parcel data = Parcel.obtain();
            Parcel reply = Parcel.obtain();
            data.writeInterfaceToken(TOKEN_AM);
            boolean ok = am.transact(4000000, data, reply, 0);
            data.recycle();
            try {
                reply.readException();
                reply.recycle();
                fail("unknown code 4000000 did not raise (ok=" + ok + ")");
            } catch (UnsupportedOperationException expected) {
                reply.recycle();
                System.out.println("PASS unknown code 4000000 -> EX_UNSUPPORTED_OPERATION (structured)");
            }
        } catch (Throwable t) {
            fail("unknown code threw " + t);
        }
    }

    static class Receiver extends Binder {
        final CountDownLatch delivered;
        Receiver(int n) { delivered = new CountDownLatch(n); }

        @Override
        protected boolean onTransact(int code, Parcel data, Parcel reply, int flags) {
            try {
                if (code != 1) {
                    return super.onTransact(code, data, reply, flags);
                }
                data.enforceInterface(TOKEN_RECEIVER);
                data.readInt(); // resultCode
                if (reply != null) {
                    reply.writeNoException();
                }
                delivered.countDown();
                return true;
            } catch (Throwable t) {
                return false;
            }
        }
    }

    static void stepReverseDeliveries(int n) {
        try {
            int rc = Nat.startBinderLooper();
            if (rc != 1) {
                fail("startBinderLooper -> " + rc);
                return;
            }
            int half = n / 2;
            Receiver registered = new Receiver(half);
            Receiver parcelOnly = new Receiver(n - half);

            // Exercise IServiceManager.addService for real (the peer registers it).
            Class<?> sm = Class.forName("android.os.ServiceManager");
            Method add = sm.getDeclaredMethod("addService", String.class, IBinder.class);
            add.setAccessible(true);
            add.invoke(null, "acos.java", registered);

            IBinder autofill = service("autofill");
            if (autofill == null) {
                fail("no autofill service");
                return;
            }
            // Half the deliveries go to the addService-registered binder, half to
            // a binder that was NEVER registered and only ever travelled inside a
            // parcel (the P2d SIGSEGV case: an unrooted node).
            int sent = addClient(autofill, n, registered, parcelOnly);
            if (sent < 0) {
                return;
            }
            boolean okRegistered = registered.delivered.await(10, TimeUnit.SECONDS);
            boolean okParcel = parcelOnly.delivered.await(10, TimeUnit.SECONDS);
            long missRegistered = registered.delivered.getCount();
            long missParcel = parcelOnly.delivered.getCount();
            if (okRegistered && okParcel && missRegistered == 0 && missParcel == 0) {
                System.out.println("PASS " + n + " reverse IResultReceiver.send deliveries ("
                        + half + " addService-registered, " + (n - half)
                        + " parcel-only binder), no race failure");
            } else {
                fail("reverse deliveries: registered " + (half - missRegistered) + "/" + half
                        + ", parcel-only " + ((n - half) - missParcel) + "/" + (n - half));
            }
        } catch (Throwable t) {
            fail("reverse deliveries threw " + t);
        }
    }

    /** Fire `n` oneway IAutoFillManager.addClient calls; -1 on failure. */
    static int addClient(IBinder autofill, int n, Receiver registered, Receiver parcelOnly) {
        int half = n / 2;
        for (int i = 0; i < n; i++) {
            // The service answers by delivering to the LAST argument.
            Receiver target = i < half ? registered : parcelOnly;
            try {
                Parcel data = Parcel.obtain();
                Parcel reply = Parcel.obtain();
                data.writeInterfaceToken(TOKEN_AUTOFILL);
                data.writeStrongBinder(registered);   // IAutoFillManagerClient
                data.writeTypedObject(null, 0);       // ComponentName
                data.writeInt(0);                     // userId
                data.writeStrongBinder(target);       // IResultReceiver (delivery target)
                // oneway: the service answers by delivering IResultReceiver.send
                autofill.transact(CODE_AUTOFILL_ADD_CLIENT, data, reply,
                        IBinder.FLAG_ONEWAY);
                data.recycle();
                reply.recycle();
            } catch (Throwable t) {
                fail("addClient(" + i + ") threw " + t);
                return -1;
            }
        }
        return 0;
    }

    static void fail(String what) {
        failures++;
        System.out.println("FAIL " + what);
    }
}
