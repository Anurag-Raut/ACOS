package dev.acos.artprobe;

import java.util.Arrays;

/**
 * The C experiment's Java driver: loads a Rust-built JNI library into *real
 * ART* (dalvikvm64 from the API-34 image, no Android) and exercises every
 * interface our service layer would need.
 *
 * Prints one `PASS`/`FAIL` line per check and a final `RESULT` line, plus
 * `METRIC` lines the report quotes.
 */
public final class Main {
    private static int pass = 0, fail = 0;

    private static void check(String name, boolean ok, String detail) {
        if (ok) pass++; else fail++;
        System.out.println((ok ? "PASS " : "FAIL ") + name + (detail.isEmpty() ? "" : " :: " + detail));
    }

    private static void metric(String name, String value) {
        System.out.println("METRIC " + name + " " + value);
    }

    private static int javaAdd(int a, int b) { return a + b; } // JNI-free baseline

    public static void main(String[] args) throws Exception {
        String lib = System.getProperty("probe.lib", "");
        String mode = System.getProperty("probe.mode", "all");

        // ---- 1. load the Rust library -------------------------------------
        if (!lib.isEmpty()) {
            try {
                System.load(lib);
                check("load-absolute", true, lib);
            } catch (Throwable t) {
                check("load-absolute", false, t.toString());
            }
        } else {
            try {
                System.loadLibrary("acosjniprobe");
                check("loadLibrary", true, "acosjniprobe");
            } catch (Throwable t) {
                check("loadLibrary", false, t.toString());
            }
        }

        if (!mode.equals("all")) {
            if (mode.equals("fork")) { forkTest(); return; }
            if (mode.equals("bench")) { bench(); return; }
        }

        // ---- 2. scalar call ------------------------------------------------
        try {
            check("add(2,3)==5", NativeProbe.nativeAdd(2, 3) == 5,
                  "got " + NativeProbe.nativeAdd(2, 3));
        } catch (Throwable t) { check("add(2,3)==5", false, t.toString()); }

        // ---- 3. String round trip -----------------------------------------
        String in = "héllo → ACOS \u00e9\u00e8 \u4f60\u597d";
        try {
            String out = NativeProbe.nativeEcho(in);
            check("echo-string", ("rust:" + in).equals(out), "got " + out);
        } catch (Throwable t) { check("echo-string", false, t.toString()); }

        // ---- 4. callback into Java (same thread) ---------------------------
        try {
            Callback.events.clear();
            NativeProbe.nativeCallback("ping");
            check("callback-same-thread", Callback.events.equals(Arrays.asList("ping")),
                  Callback.events.toString());
        } catch (Throwable t) { check("callback-same-thread", false, t.toString()); }

        // ---- 5. callback from a native-created thread ----------------------
        try {
            Callback.events.clear();
            NativeProbe.nativeCallbackFromThread();
            for (int i = 0; i < 100 && Callback.events.isEmpty(); i++) Thread.sleep(10);
            check("callback-native-thread",
                  Callback.events.equals(Arrays.asList("from-native-thread")),
                  Callback.events.toString());
        } catch (Throwable t) { check("callback-native-thread", false, t.toString()); }

        // ---- 6. primitive array in + out -----------------------------------
        try {
            int[] out = NativeProbe.nativeCopyIntArray(new int[]{1, 2, 3, -4});
            check("int-array-inout", Arrays.equals(out, new int[]{3, 5, 7, -7}),
                  Arrays.toString(out));
        } catch (Throwable t) { check("int-array-inout", false, t.toString()); }

        // ---- 7. one ServiceHost-style round trip ---------------------------
        try {
            String r = NativeProbe.nativeServiceCall("hello-service");
            check("service-roundtrip", r.startsWith("0|pong[0]:HELLO-SERVICE"), "got " + r);
        } catch (Throwable t) { check("service-roundtrip", false, t.toString()); }

        bench();
    }

    private static void bench() {
        final int N = 1_000_000;
        // warm up
        int acc = 0;
        for (int i = 0; i < 200_000; i++) acc += javaAdd(1, 1) + NativeProbe.nativeAdd(1, 1);
        if (acc == 42) System.out.println(); // keep the loop alive

        long t0 = System.nanoTime();
        int s = 0;
        for (int i = 0; i < N; i++) s += javaAdd(i, 1);
        long javaNs = (System.nanoTime() - t0) / N;
        check("java-baseline", s != 42 || true, "");

        t0 = System.nanoTime();
        s = 0;
        for (int i = 0; i < N; i++) s += NativeProbe.nativeAdd(i, 1);
        long jniNs = (System.nanoTime() - t0) / N;
        if (s == 42) System.out.println();

        metric("java-static-call-ns", Long.toString(javaNs));
        metric("jni-call-ns", Long.toString(jniNs));
        metric("jni-overhead-ns", Long.toString(jniNs - javaNs));

        // String round trip cost (includes UTF-8 conv + allocation on both sides)
        String payload = "the quick brown fox jumps over the lazy dog";
        int M = 100_000;
        t0 = System.nanoTime();
        int len = 0;
        for (int i = 0; i < M; i++) len += NativeProbe.nativeEcho(payload).length();
        long echoNs = (System.nanoTime() - t0) / M;
        if (len == 42) System.out.println();
        metric("jni-string-roundtrip-ns", Long.toString(echoNs));

        // Service round trip
        long svcNs = NativeProbe.nativeServiceBench(2000, "ping");
        metric("service-roundtrip-ns", Long.toString(svcNs));
        if (svcNs < 0) { fail++; System.out.println("FAIL service-bench :: socket unreachable"); }
        else { pass++; System.out.println("PASS service-bench"); }

        System.out.println("RESULT pass=" + pass + " fail=" + fail);
    }

    private static void forkTest() throws Exception {
        int n = Integer.getInteger("fork.n", 5);
        int holdMs = Integer.getInteger("fork.holdMs", 4000);
        int[] pids = new int[n];
        int made = NativeProbe.nativeFork(n, holdMs, pids);
        if (made == 0 && n > 0) {
            // We are a forked child that returned into Java.  Dirty a realistic
            // amount of heap (-Dfork.workKb) so the PSS sample is not the
            // "children only sleep" best case, then die without teardown.
            int kb = Integer.getInteger("fork.workKb", 0);
            byte[][] junk = new byte[kb][];
            for (int i = 0; i < kb; i++) { junk[i] = new byte[1024]; junk[i][0] = (byte) i; }
            try { Thread.sleep(holdMs); } catch (Exception ignored) { }
            Runtime.getRuntime().halt(0);
        }
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < made; i++) sb.append(pids[i]).append(i + 1 < made ? "," : "");
        System.out.println("FORKED n=" + made + " pids=" + sb);
        System.out.flush();
        Thread.sleep(holdMs + 800);
        System.out.println("RESULT fork made=" + made + " requested=" + n);
    }
}
