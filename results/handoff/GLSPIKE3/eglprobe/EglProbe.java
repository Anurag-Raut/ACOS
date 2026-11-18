// GLSPIKE3 diagnostic: raw EGL14 call sequence against whatever driver
// ro.hardware.egl names, printing the result of every step, the EGL strings the
// driver reports, and /proc/self/maps before/after each step (so "did the
// Vulkan ICD get mapped" is observed, not inferred). Diagnostic only: no GL
// code, no rendering.

import android.opengl.EGL14;
import android.opengl.EGLConfig;
import android.opengl.EGLContext;
import android.opengl.EGLDisplay;
import android.opengl.EGLSurface;
import java.io.BufferedReader;
import java.io.FileReader;
import java.lang.reflect.Method;

public final class EglProbe {
    static final String T = "EglProbe: ";

    static void p(String s) { System.out.println(T + s); System.out.flush(); }

    static String mapsSnapshot(String label) {
        StringBuilder out = new StringBuilder();
        int total = 0;
        try (BufferedReader r = new BufferedReader(new FileReader("/proc/self/maps"))) {
            String line;
            while ((line = r.readLine()) != null) {
                total++;
                if (line.contains("vulkan") || line.contains("libEGL")
                        || line.contains("GLES") || line.contains("libhardware")
                        || line.contains("libhidl") || line.contains("mapper")
                        || line.contains("angle") || line.contains("libcutils")
                        || line.contains("libnativewindow") || line.contains("libsync")) {
                    int slash = line.indexOf('/');
                    if (slash >= 0) {
                        String path = line.substring(slash).trim();
                        if (!out.toString().contains(path + " ")) out.append(path).append(' ');
                    }
                }
            }
        } catch (Throwable e) {
            return label + " maps_failed=" + e;
        }
        p("maps " + label + " lines=" + total + " libs=" + out.toString().trim());
        return out.toString();
    }

    static void dir(String path) {
        java.io.File f = new java.io.File(path);
        String[] names = f.list();
        p("dir " + path + " exists=" + f.exists() + " names="
                + (names == null ? "null" : String.join(",", names)));
    }

    static String prop(String key) {
        try {
            Class<?> c = Class.forName("android.os.SystemProperties");
            Method m = c.getMethod("get", String.class);
            Object v = m.invoke(null, key);
            return String.valueOf(v);
        } catch (Throwable e) {
            return "<reflection failed: " + e + ">";
        }
    }

    public static void main(String[] args) {
        p("begin pid=" + android.os.Process.myPid());
        p("prop ro.hardware.egl=" + prop("ro.hardware.egl")
                + " ro.hardware.vulkan=" + prop("ro.hardware.vulkan")
                + " ro.board.platform=" + prop("ro.board.platform")
                + " persist.graphics.egl=" + prop("persist.graphics.egl"));
        dir("/vendor/lib64");
        dir("/vendor/lib64/hw");
        dir("/vendor/lib64/egl");
        mapsSnapshot("start");

        EGLDisplay dpy = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY);
        p("eglGetDisplay -> " + dpy + " (no_display=" + (dpy == EGL14.EGL_NO_DISPLAY)
                + ") err=" + EGL14.eglGetError());
        if (dpy == EGL14.EGL_NO_DISPLAY) { p("STOP no display"); return; }

        int[] major = new int[1];
        int[] minor = new int[1];
        boolean ok = EGL14.eglInitialize(dpy, major, 0, minor, 0);
        int err = EGL14.eglGetError();
        p("eglInitialize -> " + ok + " version=" + major[0] + "." + minor[0] + " err=" + err);
        mapsSnapshot("after eglInitialize");
        if (!ok) { p("STOP eglInitialize false"); return; }

        p("EGL_VENDOR=" + EGL14.eglQueryString(dpy, EGL14.EGL_VENDOR));
        p("EGL_VERSION=" + EGL14.eglQueryString(dpy, EGL14.EGL_VERSION));
        p("EGL_CLIENT_APIS=" + EGL14.eglQueryString(dpy, EGL14.EGL_CLIENT_APIS));
        String ext = EGL14.eglQueryString(dpy, EGL14.EGL_EXTENSIONS);
        p("EGL_DISPLAY_EXTENSIONS=" + ext);

        // hwui's load8BitsConfig, first with PRESERVED (what the guest log shows
        // failing) and then without.
        int[] base = {
            EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT,
            EGL14.EGL_RED_SIZE, 8, EGL14.EGL_GREEN_SIZE, 8,
            EGL14.EGL_BLUE_SIZE, 8, EGL14.EGL_ALPHA_SIZE, 8,
            EGL14.EGL_DEPTH_SIZE, 0,
            EGL14.EGL_CONFIG_CAVEAT, EGL14.EGL_NONE,
            EGL14.EGL_STENCIL_SIZE, 0,
            EGL14.EGL_SURFACE_TYPE, EGL14.EGL_WINDOW_BIT,
            EGL14.EGL_NONE,
        };
        int[] preserved = new int[base.length + 2];
        System.arraycopy(base, 0, preserved, 0, base.length - 1);
        preserved[base.length - 1] = EGL14.EGL_SURFACE_TYPE;
        preserved[base.length] = EGL14.EGL_WINDOW_BIT | EGL14.EGL_SWAP_BEHAVIOR_PRESERVED_BIT;
        preserved[base.length + 1] = EGL14.EGL_NONE;

        EGLConfig[] cfgs = new EGLConfig[1];
        int[] n = new int[1];
        boolean cs = EGL14.eglChooseConfig(dpy, preserved, 0, cfgs, 0, 1, n, 0);
        p("eglChooseConfig(preserved) -> " + cs + " num=" + n[0] + " cfg=" + cfgs[0]
                + " err=" + EGL14.eglGetError());
        cfgs = new EGLConfig[1];
        n = new int[1];
        cs = EGL14.eglChooseConfig(dpy, base, 0, cfgs, 0, 1, n, 0);
        p("eglChooseConfig(window, no-preserved) -> " + cs + " num=" + n[0] + " cfg=" + cfgs[0]
                + " err=" + EGL14.eglGetError());

        int[] ctxAttr = {EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE};
        EGLContext ctx = EGL14.eglCreateContext(dpy, cfgs[0], EGL14.EGL_NO_CONTEXT, ctxAttr, 0);
        err = EGL14.eglGetError();
        p("eglCreateContext -> " + ctx + " isNoContext=" + (ctx == EGL14.EGL_NO_CONTEXT)
                + " err=" + err + " (" + errName(err) + ")");
        mapsSnapshot("after eglCreateContext");

        if (ctx != EGL14.EGL_NO_CONTEXT) {
            int[] pb = {EGL14.EGL_WIDTH, 1, EGL14.EGL_HEIGHT, 1, EGL14.EGL_NONE};
            EGLSurface s = EGL14.eglCreatePbufferSurface(dpy, cfgs[0], pb, 0);
            p("eglCreatePbufferSurface -> " + s + " err=" + EGL14.eglGetError());
            boolean mc = EGL14.eglMakeCurrent(dpy, s, s, ctx);
            p("eglMakeCurrent -> " + mc + " err=" + EGL14.eglGetError());
            p("glGetString via GLES20 vendor=" + android.opengl.GLES20.glGetString(0x1F00)
                    + " version=" + android.opengl.GLES20.glGetString(0x1F02)
                    + " renderer=" + android.opengl.GLES20.glGetString(0x1F01));
            mapsSnapshot("after makeCurrent");
        }

        // Does this ANGLE expose EGL_ANGLE_platform_angle, and can it be driven to
        // a Vulkan display explicitly? Constants from EGL_ANGLE_platform_angle.
        if (ext != null && ext.contains("EGL_ANGLE_platform_angle")) {
            try {
                Method m = EGL14.class.getMethod("eglGetPlatformDisplayEXT", int.class,
                        Object.class, int[].class, int.class);
                int[][] trials = {
                    {0x3201, 0x3203},          // TYPE = VULKAN
                    {0x3201, 0x3209},          // TYPE = SWIFTSHADER
                    {0x3201, 0x3202},          // TYPE = OPENGL
                };
                for (int[] t : trials) {
                    int[] attribs = {t[0], t[1], EGL14.EGL_NONE};
                    Object d = m.invoke(null, 0x3200 /*EGL_PLATFORM_ANGLE_ANGLE*/,
                            EGL14.EGL_DEFAULT_DISPLAY, attribs, 0);
                    p("eglGetPlatformDisplayEXT(type=" + Integer.toHexString(t[1]) + ") -> " + d
                            + " err=" + EGL14.eglGetError());
                }
            } catch (Throwable e) {
                p("eglGetPlatformDisplayEXT unavailable: " + e);
            }
        } else {
            p("EGL_ANGLE_platform_angle: absent from EGL_DISPLAY_EXTENSIONS");
        }
        p("end");
    }

    static String errName(int e) {
        switch (e) {
            case 0x3000: return "EGL_SUCCESS";
            case 0x3001: return "EGL_NOT_INITIALIZED";
            case 0x3002: return "EGL_BAD_ACCESS";
            case 0x3003: return "EGL_BAD_ALLOC";
            case 0x3004: return "EGL_BAD_ATTRIBUTE";
            case 0x3005: return "EGL_BAD_CONFIG";
            case 0x3006: return "EGL_BAD_CONTEXT";
            case 0x3007: return "EGL_BAD_CURRENT_SURFACE";
            case 0x3008: return "EGL_BAD_DISPLAY";
            case 0x3009: return "EGL_BAD_MATCH";
            case 0x300A: return "EGL_BAD_NATIVE_PIXMAP";
            case 0x300B: return "EGL_BAD_NATIVE_WINDOW";
            case 0x300C: return "EGL_BAD_PARAMETER";
            case 0x300D: return "EGL_BAD_SURFACE";
            case 0x300E: return "EGL_CONTEXT_LOST";
            default: return "?";
        }
    }
}
