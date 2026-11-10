import android.os.Build;
import android.os.Process;
import android.os.SystemClock;
import android.os.SystemProperties;
import android.os.Trace;
import android.util.Log;

/**
 * Java-level proof for the {@code core.os} natives (T-25).
 *
 * <p>{@code android.os.SystemProperties} is {@code @hide}, so the test is
 * compiled against {@code crates/acos-vm/tests/java/stubs/android/os/SystemProperties.java}
 * (signatures only) and only {@code OsNativesMain.class} is fed to d8: the DEX
 * therefore references {@code Landroid/os/SystemProperties;} by name and the VM
 * binds the *real* framework class from {@code framework.jar}, not the stub.
 */
public class OsNativesMain {
    public static void main(String[] args) {
        // ---- Build identity: read from build.prop through SystemProperties.
        System.out.println("sdk=" + Build.VERSION.SDK_INT);
        System.out.println("sdk_string=" + Build.VERSION.SDK);
        System.out.println("release=" + Build.VERSION.RELEASE);
        System.out.println("abi=" + Build.CPU_ABI);
        System.out.println("abis=" + join(Build.SUPPORTED_ABIS));
        System.out.println("abi32_count=" + Build.SUPPORTED_32_BIT_ABIS.length);
        System.out.println("abi64_count=" + Build.SUPPORTED_64_BIT_ABIS.length);
        System.out.println("fingerprint=" + Build.FINGERPRINT);
        System.out.println("brand=" + Build.BRAND);
        System.out.println("device=" + Build.DEVICE);
        System.out.println("model=" + Build.MODEL);
        System.out.println("product=" + Build.PRODUCT);
        System.out.println("id=" + Build.ID);
        System.out.println("type=" + Build.TYPE);
        System.out.println("tags=" + Build.TAGS);

        // ---- Unset key: every accessor returns the caller's default.
        System.out.println("get_empty=[" + SystemProperties.get("acos.unknown.key") + "]");
        System.out.println("get_default=" + SystemProperties.get("acos.unknown.key", "fallback"));
        System.out.println("get_int=" + SystemProperties.getInt("acos.unknown.key", 42));
        System.out.println("get_long=" + SystemProperties.getLong("acos.unknown.key", -7L));
        System.out.println("get_boolean_true=" + SystemProperties.getBoolean("acos.unknown.key", true));
        System.out.println("get_boolean_false=" + SystemProperties.getBoolean("acos.unknown.key", false));
        // ---- A set key, and a set-but-not-numeric key.
        System.out.println("sdk_prop=" + SystemProperties.get("ro.build.version.sdk"));
        System.out.println("sdk_prop_int=" + SystemProperties.getInt("ro.build.version.sdk", -1));
        System.out.println("type_prop_int=" + SystemProperties.getInt("ro.build.type", -1));

        // ---- SystemClock.
        long uptime = SystemClock.uptimeMillis();
        System.out.println("uptime_nonneg=" + (uptime >= 0));
        System.out.println("elapsed_ns_pos=" + (SystemClock.elapsedRealtimeNanos() > 0));
        System.out.println("uptime_ns_pos=" + (SystemClock.uptimeNanos() > 0));
        System.out.println("elapsed_ge_uptime="
                + (SystemClock.elapsedRealtime() >= SystemClock.uptimeMillis()));
        System.out.println("thread_time_nonneg=" + (SystemClock.currentThreadTimeMillis() >= 0));
        long wall_micro = System.currentTimeMillis() * 1000L;
        long vm_micro = SystemClock.currentTimeMicro();
        System.out.println("micro_close="
                + (vm_micro - wall_micro < 10000000L && wall_micro - vm_micro < 10000000L));

        // ---- Process identity and the advisory priority table.
        System.out.println("pid_pos=" + (Process.myPid() > 0));
        System.out.println("tid_pos=" + (Process.myTid() > 0));
        System.out.println("uid=" + Process.myUid());
        System.out.println("uid_for_name_system=" + Process.getUidForName("system"));
        System.out.println("uid_for_name_unknown=" + Process.getUidForName("acos.no.such.user"));
        int tid = Process.myTid();
        Process.setThreadPriority(Process.THREAD_PRIORITY_BACKGROUND);
        System.out.println("priority=" + Process.getThreadPriority(tid));

        // ---- Trace: callable, but nothing is enabled.
        System.out.println("trace_enabled=" + Trace.isEnabled());
        Trace.beginSection("acos-test");
        Trace.endSection();

        // ---- Log: the Rust test checks the exact logcat line.
        int logged = Log.i("AcosFixture", "onCreate");
        System.out.println("logged=" + (logged > 0));
        System.out.println("loggable_info=" + Log.isLoggable("AcosFixture", Log.INFO));
        System.out.println("loggable_verbose=" + Log.isLoggable("AcosFixture", Log.VERBOSE));
        Log.w("AcosFixture", "warn");
        System.out.println("done");
    }

    private static String join(String[] values) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < values.length; i++) {
            if (i > 0) {
                sb.append(',');
            }
            sb.append(values[i]);
        }
        return sb.toString();
    }
}
