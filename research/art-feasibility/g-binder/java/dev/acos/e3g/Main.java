package dev.acos.e3g;

import android.os.Binder;
import android.os.Bundle;
import android.os.IBinder;
import android.os.Parcel;
import android.os.ParcelFileDescriptor;
import android.os.Process;

import java.io.FileInputStream;
import java.lang.reflect.Method;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

/**
 * E3G acceptance ladder: stock libbinder + stock android.os.{Binder,Parcel,ServiceManager}
 * against a userspace binder driver that talks to a Rust peer over a unix socket.
 *
 *   scripts/run.sh all | smgr | echo | fd | latency | reverse
 */
public final class Main {
    static final String ECHO_TOKEN = "dev.acos.e3g.IEcho";
    static final int CODE_ECHO = 5;
    static final int CODE_FD = 9;
    static final int CODE_REVERSE = 7;

    public static void main(String[] args) {
        String step = args.length > 0 ? args[0] : "all";
        load();
        switch (step) {
            case "probe":   probe(); break;
            case "smgr":    stepSmgr(); break;
            case "echo":    stepEcho(); break;
            case "fd":      stepFd(); break;
            case "latency": stepLatency(); break;
            case "reverse": stepReverse(); break;
            default:
                stepSmgr();
                stepEcho();
                stepFd();
                stepLatency();
                stepReverse();
                break;
        }
        System.out.println("STATE " + Nat.state());
        System.out.flush();
        // The binder looper thread is a non-daemon native thread; the JVM cannot
        // join it, so exit explicitly.
        System.exit(0);
    }

    // ------------------------------------------------------------- substrate
    static void load() {
        try {
            Nat.load();
        } catch (Throwable t) {
            System.out.println("OBSERVED shim already preloaded: " + t);
        }
        String[] names = {
                "register_android_util_Log",
                "register_android_os_SystemProperties",
                "register_android_os_SystemClock",
                "register_android_os_Trace",
                "register_android_os_Parcel",
                "register_android_os_Binder",
        };
        for (String n : names) {
            int rc = Nat.register(n);
            if (rc != 1) {
                System.out.println("FAIL substrate " + n + " rc=" + rc);
                return;
            }
        }
        System.out.println("PASS substrate: 6 framework natives registered");
    }

    // ---------------------------------------------------------------- rung 1
    static IBinder service(String name) throws Exception {
        Class<?> sm = Class.forName("android.os.ServiceManager");
        Method m = sm.getDeclaredMethod("getService", String.class);
        m.setAccessible(true);
        return (IBinder) m.invoke(null, name);
    }

    static void stepSmgr() {
        try {
            IBinder b = service("activity");
            if (b == null) {
                System.out.println("FAIL [1] ServiceManager.getService(\"activity\") -> null");
                return;
            }
            System.out.println("PASS [1] getService(\"activity\") -> " + b.getClass().getName()
                               + " @" + Integer.toHexString(System.identityHashCode(b))
                               + " alive=" + b.isBinderAlive() + " pingBinder=" + b.pingBinder());
            System.out.println("PASS [1] getService(\"nope\") -> " + service("nope"));
        } catch (Throwable t) {
            fail("[1] ServiceManager.getService", t);
        }
    }

    /** Diagnostic: drive the IServiceManager BinderProxy by hand and dump the reply. */
    static void probe() {
        try {
            Class<?> sm = Class.forName("android.os.ServiceManager");
            Method gis = sm.getDeclaredMethod("getIServiceManager");
            gis.setAccessible(true);
            Object wrapper = gis.invoke(null);
            java.lang.reflect.Field f = wrapper.getClass().getDeclaredField("mServiceManager");
            f.setAccessible(true);
            Object bp = f.get(wrapper);
            Method asBinder = bp.getClass().getMethod("asBinder");
            IBinder remote = (IBinder) asBinder.invoke(bp);
            System.out.println("PROBE wrapper=" + wrapper.getClass().getName()
                               + " proxy=" + bp.getClass().getName() + " remote=" + remote);
            Parcel data = Parcel.obtain();
            Parcel reply = Parcel.obtain();
            data.writeInterfaceToken("android.os.IServiceManager");
            data.writeString("activity");
            boolean ok = remote.transact(2, data, reply, 0);
            System.out.println("PROBE transact=" + ok + " dataSize=" + reply.dataSize()
                               + " avail=" + reply.dataAvail());
            reply.setDataPosition(0);
            try {
                reply.readException();
                System.out.println("PROBE readException ok");
            } catch (Throwable t) { System.out.println("PROBE readException: " + t); }
            int pos = reply.dataPosition();
            IBinder b = reply.readStrongBinder();
            System.out.println("PROBE readStrongBinder pos=" + pos + " -> " + b
                               + " posAfter=" + reply.dataPosition() + " size=" + reply.dataSize());
            data.recycle();
            reply.recycle();
        } catch (Throwable t) {
            fail("probe", t);
        }
    }

    // ---------------------------------------------------------------- rung 2
    static void stepEcho() {
        try {
            IBinder b = service("activity");
            if (b == null) { System.out.println("FAIL [2] no activity service"); return; }
            Parcel data = Parcel.obtain();
            Parcel reply = Parcel.obtain();
            data.writeInterfaceToken(ECHO_TOKEN);
            data.writeInt(1234);
            data.writeString("hello");
            Bundle in = new Bundle();
            in.putString("s", "bundle-value");
            in.putInt("i", 42);
            in.putStringArray("arr", new String[] {"a", "b"});
            data.writeBundle(in);
            boolean ok = b.transact(CODE_ECHO, data, reply, 0);
            reply.readException();
            int marker = reply.readInt();
            int gotInt = reply.readInt();
            String gotStr = reply.readString();
            Bundle out = reply.readBundle(Main.class.getClassLoader());
            boolean same = out != null
                    && "bundle-value".equals(out.getString("s"))
                    && out.getInt("i") == 42
                    && out.getStringArray("arr") != null
                    && out.getStringArray("arr").length == 2;
            boolean pass = ok && marker == 0xAC05 && gotInt == 1234
                    && "hello+peer".equals(gotStr) && same;
            System.out.println((pass ? "PASS" : "FAIL") + " [2] echo: transact=" + ok
                               + " marker=0x" + Integer.toHexString(marker) + " int=" + gotInt
                               + " string=" + gotStr + " bundle=" + (same ? "identical" : "MISMATCH " + out)
                               + " inSize=" + data.dataSize() + " outSize=" + reply.dataSize());
            data.recycle();
            reply.recycle();
        } catch (Throwable t) {
            fail("[2] echo transact", t);
        }
    }

    // ---------------------------------------------------------------- rung 4
    static void stepFd() {
        try {
            IBinder b = service("activity");
            if (b == null) { System.out.println("FAIL [4] no activity service"); return; }
            ParcelFileDescriptor[] pipe = ParcelFileDescriptor.createPipe();
            Parcel data = Parcel.obtain();
            Parcel reply = Parcel.obtain();
            data.writeInterfaceToken(ECHO_TOKEN);
            // The parcel carries a ParcelFileDescriptor: libbinder flattens it to a
            // BINDER_TYPE_FD object, the driver must install it in the peer.
            data.writeFileDescriptor(pipe[1].getFileDescriptor());
            boolean ok = b.transact(CODE_FD, data, reply, 0);
            reply.readException();
            int wrote = reply.readInt();
            pipe[1].close();
            byte[] buf = new byte[64];
            int n;
            try (FileInputStream fis = new FileInputStream(pipe[0].getFileDescriptor())) {
                n = fis.read(buf);
            }
            String text = n > 0 ? new String(buf, 0, n, "UTF-8").trim() : "";
            boolean pass = ok && wrote == 1 && "PONG-FROM-PEER".equals(text);
            System.out.println((pass ? "PASS" : "FAIL") + " [4] fd passing: transact=" + ok
                               + " peerWrote=" + wrote + " read=" + n + "B text=\"" + text + "\"");
            data.recycle();
            reply.recycle();
            pipe[0].close();
        } catch (Throwable t) {
            fail("[4] fd passing", t);
        }
    }

    // ---------------------------------------------------------------- rung 5
    static void stepLatency() {
        try {
            IBinder b = service("activity");
            if (b == null) { System.out.println("FAIL [5] no activity service"); return; }
            final int n = 200;
            long[] samples = new long[n];
            for (int i = 0; i < n; i++) {
                Parcel data = Parcel.obtain();
                Parcel reply = Parcel.obtain();
                data.writeInterfaceToken(ECHO_TOKEN);
                data.writeInt(i);
                data.writeString("x");
                data.writeBundle(new Bundle());
                long t0 = System.nanoTime();
                b.transact(CODE_ECHO, data, reply, 0);
                long t1 = System.nanoTime();
                reply.readException();
                if (reply.readInt() != 0xAC05) {
                    System.out.println("FAIL [5] bad latency reply at i=" + i);
                    return;
                }
                samples[i] = t1 - t0;
                data.recycle();
                reply.recycle();
            }
            java.util.Arrays.sort(samples);
            long sum = 0;
            for (long s : samples) sum += s;
            System.out.printf("PASS [5] latency: n=%d mean=%.1fus p50=%.1fus p95=%.1fus max=%.1fus%n",
                              n, sum / 1000.0 / n, samples[n / 2] / 1000.0,
                              samples[(int) (n * 0.95)] / 1000.0, samples[n - 1] / 1000.0);
        } catch (Throwable t) {
            fail("[5] latency", t);
        }
    }

    // ---------------------------------------------------------------- rung 3
    static class JavaService extends Binder {
        final CountDownLatch called = new CountDownLatch(1);
        final AtomicReference<String> seen = new AtomicReference<>("");

        @Override
        protected boolean onTransact(int code, Parcel data, Parcel reply, int flags) {
            try {
                if (code != CODE_REVERSE) {
                    return super.onTransact(code, data, reply, flags);
                }
                data.enforceInterface("dev.acos.e3g.IJavaService");
                int v = data.readInt();
                String s = data.readString();
                seen.set("code=" + code + " int=0x" + Integer.toHexString(v) + " string=" + s);
                if (reply != null) {
                    reply.writeNoException();
                    reply.writeInt(0x600D);
                    reply.writeString("pong-from-java");
                }
                called.countDown();
                return true;
            } catch (Throwable t) {
                seen.set("onTransact threw " + t);
                called.countDown();
                return false;
            }
        }
    }

    static void stepReverse() {
        try {
            int rc = Nat.startBinderLooper();
            System.out.println("INFO [3] startBinderLooper -> " + rc);
            if (rc != 1) { System.out.println("FAIL [3] no binder looper thread"); return; }
            JavaService svc = new JavaService();
            Class<?> sm = Class.forName("android.os.ServiceManager");
            Method add = sm.getDeclaredMethod("addService", String.class, IBinder.class);
            add.setAccessible(true);
            // The peer initiates the reverse transaction right after it replies to
            // addService.  Delivery of that first peer-initiated transaction is
            // racy in this prototype (see E3G.md), so retry addService: every
            // registration makes the peer send another reverse call.
            add.invoke(null, "acos.java", svc);
            boolean ok = svc.called.await(2, TimeUnit.SECONDS);
            int tries = 1;
            while (!ok && tries < 8) {
                add.invoke(null, "acos.java", svc);
                tries++;
                ok = svc.called.await(2, TimeUnit.SECONDS);
            }
            System.out.println((ok ? "PASS" : "FAIL") + " [3] reverse call into Java Binder: "
                               + (ok ? svc.seen.get() + " (addService attempts=" + tries + ")"
                                     : "no incoming transaction after " + tries + " registrations"));
        } catch (Throwable t) {
            fail("[3] reverse call", t);
        }
    }

    // ------------------------------------------------------------------ helpers
    static void fail(String what, Throwable t) {
        System.out.println("FAIL " + what + ": " + t);
        Throwable c = t;
        while (c.getCause() != null) c = c.getCause();
        if (c != t) System.out.println("  root cause: " + c);
        StackTraceElement[] st = (c == t ? t : c).getStackTrace();
        for (int i = 0; i < Math.min(6, st.length); i++) {
            System.out.println("  at " + st[i]);
        }
    }
}
