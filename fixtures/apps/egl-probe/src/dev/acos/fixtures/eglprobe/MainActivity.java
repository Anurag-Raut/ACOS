package dev.acos.fixtures.eglprobe;

import android.app.Activity;
import android.opengl.EGL14;
import android.opengl.EGL15;
import android.opengl.EGLConfig;
import android.opengl.EGLContext;
import android.opengl.EGLDisplay;
import android.opengl.EGLSurface;
import android.os.Bundle;
import android.util.Log;
import android.widget.TextView;
import java.io.BufferedReader;
import java.io.FileReader;
import java.lang.reflect.Method;
import java.util.TreeSet;

/**
 * GLSPIKE4 diagnostic: the raw EGL call sequence against whatever driver
 * {@code ro.hardware.egl} names, one line per call (tag {@code EglProbe}), with
 * the relevant slice of {@code /proc/self/maps} after every step so "was the
 * Vulkan ICD mapped" is observed. Diagnostic only: no app drawing.
 */
public final class MainActivity extends Activity {
    static final String TAG = "EglProbe";

    static void p(String s) {
        // logd lines are capped near 4 KiB; chunk long strings.
        for (int i = 0; i < s.length(); i += 1500) {
            Log.i(TAG, s.substring(i, Math.min(s.length(), i + 1500)));
        }
    }

    @Override
    protected void onCreate(Bundle b) {
        super.onCreate(b);
        TextView tv = new TextView(this);
        tv.setText("EglProbe running");
        setContentView(tv);
        Thread t = new Thread(() -> {
            try {
                probe();
            } catch (Throwable e) {
                p("FATAL probe threw " + e);
            }
            p("end");
        }, "EglProbeThread");
        t.start();
    }

    static String prop(String key) {
        try {
            Class<?> c = Class.forName("android.os.SystemProperties");
            Method m = c.getMethod("get", String.class, String.class);
            return String.valueOf(m.invoke(null, key, "<unset>"));
        } catch (Throwable e) {
            return "<reflection failed: " + e + ">";
        }
    }

    static final TreeSet<String> SEEN = new TreeSet<>();

    /** Log libs newly mapped since the previous snapshot, plus the vulkan-relevant total. */
    static void maps(String label) {
        StringBuilder added = new StringBuilder();
        boolean pastel = false;
        int total = 0;
        try (BufferedReader r = new BufferedReader(new FileReader("/proc/self/maps"))) {
            String line;
            while ((line = r.readLine()) != null) {
                total++;
                int slash = line.indexOf('/');
                if (slash < 0) continue;
                String path = line.substring(slash).trim();
                if (path.contains("vulkan.pastel")) pastel = true;
                if (!path.endsWith(".so") && !path.contains(".so ")) continue;
                if (SEEN.add(path)) added.append(' ').append(path);
            }
        } catch (Throwable e) {
            p("maps " + label + " failed " + e);
            return;
        }
        p("maps[" + label + "] lines=" + total + " vulkan.pastel_mapped=" + pastel
                + " new_libs:" + (added.length() == 0 ? " (none)" : added));
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
            default: return "0x" + Integer.toHexString(e);
        }
    }

    static String err() { return errName(EGL14.eglGetError()); }

    static final int[] CONFIG = {
        EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT,
        EGL14.EGL_RED_SIZE, 8, EGL14.EGL_GREEN_SIZE, 8,
        EGL14.EGL_BLUE_SIZE, 8, EGL14.EGL_ALPHA_SIZE, 8,
        EGL14.EGL_SURFACE_TYPE, EGL14.EGL_PBUFFER_BIT,
        EGL14.EGL_NONE,
    };

    /** The full init -> config -> context -> pbuffer -> makeCurrent -> glGetString ladder. */
    static void ladder(String name, EGLDisplay dpy) {
        p(name + ": display=" + dpy + " noDisplay=" + (dpy == EGL14.EGL_NO_DISPLAY) + " err=" + err());
        if (dpy == null || dpy == EGL14.EGL_NO_DISPLAY) return;
        int[] major = new int[1], minor = new int[1];
        long t0 = System.nanoTime();
        boolean ok = EGL14.eglInitialize(dpy, major, 0, minor, 0);
        long initUs = (System.nanoTime() - t0) / 1000;
        p(name + ": eglInitialize -> " + ok + " " + major[0] + "." + minor[0] + " err=" + err()
                + " took_us=" + initUs);
        maps(name + " after eglInitialize");
        if (!ok) return;
        p(name + ": EGL_VENDOR=" + EGL14.eglQueryString(dpy, EGL14.EGL_VENDOR));
        p(name + ": EGL_VERSION=" + EGL14.eglQueryString(dpy, EGL14.EGL_VERSION));
        p(name + ": EGL_CLIENT_APIS=" + EGL14.eglQueryString(dpy, EGL14.EGL_CLIENT_APIS));
        p(name + ": EGL_EXTENSIONS=" + EGL14.eglQueryString(dpy, EGL14.EGL_EXTENSIONS));
        EGLConfig[] cfgs = new EGLConfig[1];
        int[] n = new int[1];
        boolean cs = EGL14.eglChooseConfig(dpy, CONFIG, 0, cfgs, 0, 1, n, 0);
        p(name + ": eglChooseConfig(pbuffer,es2,rgba8888) -> " + cs + " num=" + n[0] + " err=" + err());
        int[] total = new int[1];
        EGL14.eglGetConfigs(dpy, null, 0, 0, total, 0);
        p(name + ": eglGetConfigs total=" + total[0] + " err=" + err());
        if (!cs || n[0] < 1) return;
        int[] ctxAttr = {EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE};
        t0 = System.nanoTime();
        EGLContext ctx = EGL14.eglCreateContext(dpy, cfgs[0], EGL14.EGL_NO_CONTEXT, ctxAttr, 0);
        int e = EGL14.eglGetError();
        p(name + ": eglCreateContext took_us=" + (System.nanoTime() - t0) / 1000);
        p(name + ": eglCreateContext -> " + ctx + " isNoContext=" + (ctx == EGL14.EGL_NO_CONTEXT)
                + " err=" + errName(e));
        maps(name + " after eglCreateContext");
        if (ctx == EGL14.EGL_NO_CONTEXT) return;
        int[] pb = {EGL14.EGL_WIDTH, 16, EGL14.EGL_HEIGHT, 16, EGL14.EGL_NONE};
        EGLSurface s = EGL14.eglCreatePbufferSurface(dpy, cfgs[0], pb, 0);
        p(name + ": eglCreatePbufferSurface -> " + s + " err=" + err());
        boolean mc = EGL14.eglMakeCurrent(dpy, s, s, ctx);
        p(name + ": eglMakeCurrent -> " + mc + " err=" + err());
        if (mc) {
            p(name + ": GL_VENDOR=" + android.opengl.GLES20.glGetString(0x1F00)
                    + " GL_VERSION=" + android.opengl.GLES20.glGetString(0x1F02)
                    + " GL_RENDERER=" + android.opengl.GLES20.glGetString(0x1F01));
            p(name + ": GL_EXTENSIONS=" + android.opengl.GLES20.glGetString(0x1F03));
            maps(name + " after makeCurrent");
            // one real draw: clear + readback, the smallest proof of pixels.
            android.opengl.GLES20.glClearColor(1f, 0f, 0f, 1f);
            android.opengl.GLES20.glClear(0x4000);
            java.nio.ByteBuffer px = java.nio.ByteBuffer.allocateDirect(4);
            android.opengl.GLES20.glReadPixels(0, 0, 1, 1, 0x1908, 0x1401, px);
            p(name + ": glClear+glReadPixels rgba=" + (px.get(0) & 255) + "," + (px.get(1) & 255)
                    + "," + (px.get(2) & 255) + "," + (px.get(3) & 255) + " glErr=0x"
                    + Integer.toHexString(android.opengl.GLES20.glGetError()));
        }
        EGL14.eglMakeCurrent(dpy, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT);
        EGL14.eglDestroyContext(dpy, ctx);
        EGL14.eglTerminate(dpy);
    }

    static void probe() {
        p("begin pid=" + android.os.Process.myPid());
        p("props ro.hardware.egl=" + prop("ro.hardware.egl")
                + " ro.hardware.vulkan=" + prop("ro.hardware.vulkan")
                + " ro.board.platform=" + prop("ro.board.platform")
                + " persist.graphics.egl=" + prop("persist.graphics.egl")
                + " debug.angle.backend=" + prop("debug.angle.backend")
                + " ro.gfx.angle.supported=" + prop("ro.gfx.angle.supported")
                + " acos.guest.gles=" + prop("acos.guest.gles"));
        maps("start");
        p("client EGL_EXTENSIONS=" + EGL14.eglQueryString(EGL14.EGL_NO_DISPLAY, EGL14.EGL_EXTENSIONS)
                + " err=" + err());

        // 1. the display hwui gets: EGL_DEFAULT_DISPLAY through the Android loader.
        ladder("default", EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY));

        // 2. explicit ANGLE platform displays through EGL15.eglGetPlatformDisplay.
        //    Constants: ANGLE eglext_angle.h.
        long PLATFORM_ANGLE = 0x3202, TYPE = 0x3203, DEVICE = 0x3209;
        long[][] variants = {
            {TYPE, 0x3206},                    // DEFAULT
            {TYPE, 0x3450},                    // VULKAN
            {TYPE, 0x3450, DEVICE, 0x3487},    // VULKAN + SwiftShader device type
            {TYPE, 0x320E},                    // OPENGLES
            {TYPE, 0x320D},                    // OPENGL
            {TYPE, 0x34A5},                    // NULL (no HW; proves ANGLE core runs)
        };
        String[] names = {"angle-default", "angle-vulkan", "angle-vulkan-swiftshader",
            "angle-opengles", "angle-opengl", "angle-null"};
        for (int i = 0; i < variants.length; i++) {
            long[] v = variants[i];
            long[] attr = new long[v.length + 1];
            System.arraycopy(v, 0, attr, 0, v.length);
            attr[v.length] = EGL14.EGL_NONE;
            EGLDisplay d;
            try {
                d = EGL15.eglGetPlatformDisplay((int) PLATFORM_ANGLE, 0, attr, 0);
            } catch (Throwable t) {
                p(names[i] + ": eglGetPlatformDisplay threw " + t);
                continue;
            }
            ladder(names[i], d);
        }
        hardwareStage();
    }

    static String px(android.graphics.Bitmap b, int x, int y) {
        return String.format("#%08X", b.getPixel(x, y));
    }

    /** What apps actually do: HARDWARE bitmaps read back / drawn through hwui. */
    static void hardwareStage() {
        // (ii) PNG decoded straight to HARDWARE, then GPU readback via Bitmap.copy.
        try {
            android.graphics.Bitmap src = android.graphics.Bitmap.createBitmap(64, 64,
                    android.graphics.Bitmap.Config.ARGB_8888);
            for (int y = 0; y < 64; y++) {
                for (int x = 0; x < 64; x++) src.setPixel(x, y, x < 32 ? 0xFFFF0000 : 0xFF0000FF);
            }
            java.io.ByteArrayOutputStream png = new java.io.ByteArrayOutputStream();
            src.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, png);
            android.graphics.BitmapFactory.Options o = new android.graphics.BitmapFactory.Options();
            o.inPreferredConfig = android.graphics.Bitmap.Config.HARDWARE;
            byte[] pngBytes = png.toByteArray();
            android.graphics.Bitmap hw = android.graphics.BitmapFactory.decodeByteArray(
                    pngBytes, 0, pngBytes.length, o);
            p("hw: png->HARDWARE config=" + (hw == null ? "null" : hw.getConfig()));
            if (hw != null) {
                long t0 = System.nanoTime();
                p("hw: png copy(ARGB_8888) starting");
                android.graphics.Bitmap sw = hw.copy(android.graphics.Bitmap.Config.ARGB_8888, false);
                p("hw: png copy ok=" + (sw != null) + " took_us=" + (System.nanoTime() - t0) / 1000
                        + (sw == null ? "" : " left=" + px(sw, 8, 32) + " right=" + px(sw, 56, 32)));
            }
        } catch (Throwable t) {
            p("hw: png failed " + t);
        }
        // (i) a wrapped HardwareBuffer (what AccessibilityService.takeScreenshot returns).
        try {
            android.hardware.HardwareBuffer hb = android.hardware.HardwareBuffer.create(64, 64,
                    android.hardware.HardwareBuffer.RGBA_8888, 1,
                    android.hardware.HardwareBuffer.USAGE_GPU_SAMPLED_IMAGE
                            | android.hardware.HardwareBuffer.USAGE_CPU_WRITE_OFTEN);
            android.graphics.Bitmap w = android.graphics.Bitmap.wrapHardwareBuffer(hb,
                    android.graphics.ColorSpace.get(android.graphics.ColorSpace.Named.SRGB));
            p("hw: wrapHardwareBuffer config=" + (w == null ? "null" : w.getConfig()));
            if (w != null) {
                long t0 = System.nanoTime();
                p("hw: wrapped copy(ARGB_8888) starting");
                android.graphics.Bitmap sw = w.copy(android.graphics.Bitmap.Config.ARGB_8888, false);
                p("hw: wrapped copy ok=" + (sw != null) + " took_us=" + (System.nanoTime() - t0) / 1000
                        + (sw == null ? "" : " p00=" + px(sw, 0, 0)));
            }
        } catch (Throwable t) {
            p("hw: wrapped failed " + t);
        }
    }
}
