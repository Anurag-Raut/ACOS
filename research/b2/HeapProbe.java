// B2 throwaway guest probe: the heap settings ART actually installs, and how
// much of the CC collector's region space is live after a full GC.
//
// Run through the product's own ART path so the properties, preloads and
// sysroot are the ones a counter session gets:
//
//   acos-art run --dex .cache/b2/probe/classes.dex --class b2.HeapProbe --props
//
// Prints one `B2PROBE {json}` line per phase on stdout and reads its own
// /proc/self/smaps (the guest has /proc mounted). No product code is touched.

package b2;

import java.io.BufferedReader;
import java.io.FileReader;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

public final class HeapProbe {
    static final long MiB = 1024L * 1024L;

    public static void main(String[] args) throws Exception {
        int garbageMiB = 0;
        long holdMs = 0;
        boolean maps = false;
        for (int i = 0; i < args.length; i++) {
            if (args[i].equals("--garbage-mib") && i + 1 < args.length) {
                garbageMiB = Integer.parseInt(args[++i]);
            } else if (args[i].equals("--hold-ms") && i + 1 < args.length) {
                holdMs = Long.parseLong(args[++i]);
            } else if (args[i].equals("--maps")) {
                maps = true;
            }
        }

        phase("start");
        if (maps) {
            System.out.println("B2PROBE maps " + imageMaps());
        }
        // The probe's own baseline: nothing allocated by the app yet.
        System.out.println("B2PROBE phase=start " + json());
        System.out.println("B2PROBE ready");
        System.out.flush();
        if (holdMs > 0) {
            Thread.sleep(holdMs);
            System.out.println("B2PROBE phase=release " + json());
            System.out.flush();
            return;
        }

        if (garbageMiB > 0) {
            // Deliberately create garbage so the "after" reading proves the
            // collector returns the pages, not that they were never touched.
            List<byte[]> trash = new ArrayList<>();
            for (int i = 0; i < garbageMiB; i++) {
                trash.add(new byte[(int) MiB]);
            }
            for (int i = 0; i < trash.size(); i++) {
                trash.set(i, null);
            }
            trash = null;
            phase("dirty");
            System.out.println("B2PROBE phase=dirty " + json());
        }

        // Full GC: System.gc() on ART is an explicit concurrent copying GC of
        // the whole heap (not a young-gen-only collection).
        Runtime.getRuntime().gc();
        System.runFinalization();
        Runtime.getRuntime().gc();
        Thread.sleep(750);
        phase("after-gc");
        System.out.println("B2PROBE phase=after-gc " + json());

        // Second full GC after a settle: distinguishes "pages freed by the GC"
        // from "pages the collector has not unmapped yet".
        Thread.sleep(1500);
        Runtime.getRuntime().gc();
        Thread.sleep(750);
        phase("after-gc2");
        System.out.println("B2PROBE phase=after-gc2 " + json());
        System.out.flush();
    }

    static void phase(String name) {
        System.err.println("B2PROBE phase-enter " + name);
        System.err.flush();
    }

    static String json() {
        Map<String, Object> m = new LinkedHashMap<>();
        Runtime rt = Runtime.getRuntime();
        m.put("max_memory", rt.maxMemory());
        m.put("total_memory", rt.totalMemory());
        m.put("free_memory", rt.freeMemory());
        m.put("max_mib", rt.maxMemory() / (double) MiB);
        m.put("total_mib", rt.totalMemory() / (double) MiB);
        m.put("free_mib", rt.freeMemory() / (double) MiB);

        Map<String, Object> vm = vmRuntime();
        m.putAll(vm);

        Map<String, long[]> spaces = smaps();
        Map<String, Object> sm = new LinkedHashMap<>();
        for (Map.Entry<String, long[]> e : spaces.entrySet()) {
            long[] v = e.getValue(); // rss pss private_dirty shared_clean size
            Map<String, Object> one = new LinkedHashMap<>();
            one.put("rss", v[0]);
            one.put("pss", v[1]);
            one.put("private_dirty", v[2]);
            one.put("shared_clean", v[3]);
            one.put("size", v[4]);
            one.put("rss_mib", round(v[0] / (double) MiB));
            one.put("pss_mib", round(v[1] / (double) MiB));
            sm.put(e.getKey(), one);
        }
        m.put("smaps", sm);
        return toJson(m);
    }

    // dalvik.system.VMRuntime is hidden API; a plain `dalvikvm` main has no
    // targetSdk, but a blocked call must degrade to a recorded error, never a
    // silent zero.
    static Map<String, Object> vmRuntime() {
        Map<String, Object> out = new LinkedHashMap<>();
        try {
            Class<?> c = Class.forName("dalvik.system.VMRuntime");
            Method getRuntime = c.getMethod("getRuntime");
            Object vmr = getRuntime.invoke(null);
            for (String name : new String[] {
                    "getTargetHeapUtilization", "getMinimumHeapSize",
                    "getTargetSdkVersion", "getHeapGrowthLimit" }) {
                try {
                    Method m = c.getMethod(name);
                    Object v = m.invoke(vmr);
                    out.put("vm_" + name, v);
                } catch (Throwable t) {
                    out.put("vm_" + name, "unavailable:" + t.getClass().getSimpleName());
                }
            }
            // The two heap-limit properties AndroidRuntime parses are private
            // fields of VMRuntime in some releases; read them when present.
            for (String f : new String[] { "mGrowthLimit", "mMinimumHeapSize" }) {
                try {
                    java.lang.reflect.Field fld = c.getDeclaredField(f);
                    fld.setAccessible(true);
                    out.put("vm_field_" + f, fld.get(vmr));
                } catch (Throwable t) {
                    // absent on this release: not an error
                }
            }
        } catch (Throwable t) {
            out.put("vm_runtime", "unavailable:" + t.getClass().getSimpleName()
                    + ":" + t.getMessage());
        }
        return out;
    }

    // Sum the CC spaces from /proc/self/smaps. `dalvik-main space (region space)`
    // is the 256 MiB * 2 reservation the advisor matched to heap.cc:642; the
    // other dalvik-* anon mappings are reported separately so a total is
    // possible without hiding what is what.
    static Map<String, long[]> smaps() {
        Map<String, long[]> per = new LinkedHashMap<>();
        String cur = null;
        long[] acc = null;
        try (BufferedReader r = new BufferedReader(new FileReader("/proc/self/smaps"))) {
            String line;
            while ((line = r.readLine()) != null) {
                int dash = line.indexOf('-');
                int sp = line.indexOf(' ');
                if (dash > 0 && sp > dash && line.substring(0, dash).matches("[0-9a-f]+")) {
                    // header line: start-end perms offset dev inode path
                    String[] parts = line.split("\\s+", 6);
                    String path = parts.length > 5 ? parts[5] : "";
                    if (path.startsWith("[anon:dalvik-")) {
                        cur = path;
                        acc = per.computeIfAbsent(cur, k -> new long[5]);
                    } else {
                        cur = null;
                        acc = null;
                    }
                    continue;
                }
                if (acc == null) {
                    continue;
                }
                int colon = line.indexOf(':');
                if (colon < 0) {
                    continue;
                }
                String key = line.substring(0, colon);
                String val = line.substring(colon + 1).trim();
                long kb;
                try {
                    kb = Long.parseLong(val.split("\\s+")[0]);
                } catch (RuntimeException e) {
                    continue;
                }
                switch (key) {
                    case "Size": acc[4] += kb * 1024L; break;
                    case "Rss": acc[0] += kb * 1024L; break;
                    case "Pss": acc[1] += kb * 1024L; break;
                    case "Private_Dirty": acc[2] += kb * 1024L; break;
                    case "Shared_Clean": acc[3] += kb * 1024L; break;
                    default: break;
                }
            }
        } catch (Exception e) {
            per.put("error", new long[] { -1, -1, -1, -1, -1 });
        }
        return per;
    }

    // Where the boot image and the CC region space actually live in this
    // process: a file-backed .art mapping means the image was file-mapped (the
    // uncompressed case); a `dalvik-/...boot...art` anon mapping means ART
    // decompressed it. The start address answers the relocation question: an
    // image mapped at its recorded base (0x70000000 here) needs no fixups.
    static String imageMaps() {
        Map<String, Object> out = new LinkedHashMap<>();
        Map<String, String> first = new LinkedHashMap<>();
        try (BufferedReader r = new BufferedReader(new FileReader("/proc/self/maps"))) {
            String line;
            while ((line = r.readLine()) != null) {
                String[] parts = line.split("\\s+", 6);
                if (parts.length < 6) {
                    continue;
                }
                String start = parts[0].split("-")[0];
                String path = parts[5];
                String key = null;
                if (path.endsWith(".art")) {
                    key = path;
                } else if (path.startsWith("[anon:dalvik-") && path.contains("boot")
                        && path.endsWith(".art]")) {
                    key = "anon:" + path.substring("[anon:dalvik-".length());
                } else if (path.equals("[anon:dalvik-main space (region space)]")) {
                    key = "region-space";
                }
                if (key != null && !first.containsKey(key)) {
                    first.put(key, start);
                }
            }
        } catch (Exception e) {
            out.put("error", e.toString());
        }
        out.put("bases", first);
        return toJson(out);
    }

    static double round(double v) {
        return Math.round(v * 100.0) / 100.0;
    }

    // Minimal JSON writer: only maps, strings, numbers, booleans.
    static String toJson(Object o) {
        if (o == null) {
            return "null";
        }
        if (o instanceof String) {
            StringBuilder b = new StringBuilder("\"");
            for (char c : ((String) o).toCharArray()) {
                if (c == '"' || c == '\\') {
                    b.append('\\').append(c);
                } else if (c < 0x20) {
                    b.append(String.format("\\u%04x", (int) c));
                } else {
                    b.append(c);
                }
            }
            return b.append('"').toString();
        }
        if (o instanceof Number || o instanceof Boolean) {
            return String.valueOf(o);
        }
        if (o instanceof Map) {
            StringBuilder b = new StringBuilder("{");
            boolean first = true;
            for (Map.Entry<?, ?> e : ((Map<?, ?>) o).entrySet()) {
                if (!first) {
                    b.append(',');
                }
                first = false;
                b.append(toJson(String.valueOf(e.getKey()))).append(':')
                        .append(toJson(e.getValue()));
            }
            return b.append('}').toString();
        }
        if (o instanceof Object[]) {
            StringBuilder b = new StringBuilder("[");
            Object[] a = (Object[]) o;
            for (int i = 0; i < a.length; i++) {
                if (i > 0) {
                    b.append(',');
                }
                b.append(toJson(a[i]));
            }
            return b.append(']').toString();
        }
        return toJson(String.valueOf(o));
    }

    private HeapProbe() {}
}
