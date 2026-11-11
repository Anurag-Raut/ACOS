package dev.acos.e2;

import android.graphics.*;
import java.lang.reflect.*;
import java.util.*;

/** Real ART + real framework android.graphics.* + real libhwui Skia (CPU) canvas. */
public class Main {
    static StringBuilder sb = new StringBuilder();

    static String desc(Class<?> c) {
        if (c == void.class) return "V";
        if (c == boolean.class) return "Z";
        if (c == byte.class) return "B";
        if (c == char.class) return "C";
        if (c == short.class) return "S";
        if (c == int.class) return "I";
        if (c == long.class) return "J";
        if (c == float.class) return "F";
        if (c == double.class) return "D";
        if (c.isArray()) return "[" + desc(c.getComponentType());
        return "L" + c.getName().replace('.', '/') + ";";
    }

    static String sig(Method m) {
        sb.setLength(0);
        sb.append('(');
        for (Class<?> p : m.getParameterTypes()) sb.append(desc(p));
        sb.append(')');
        sb.append(desc(m.getReturnType()));
        return sb.toString();
    }

    public static void main(String[] args) throws Exception {
        System.load("/data/lib/libacosgraphicsbridge.so");
        int avail = Bridge.nativeCount();
        System.out.println("libhwui native entries extracted: " + avail);

        int rrc = Bridge.registerReal();
        System.out.println("register_android_graphics_classes rc=" + rrc);

        long t0 = System.nanoTime();
        final int W = 64, H = 64;
        Bitmap bm = Bitmap.createBitmap(W, H, Bitmap.Config.ARGB_8888);
        Canvas c = new Canvas(bm);
        Paint p = new Paint();
        p.setColor(0xFFFF0000);
        c.drawRect(4, 4, 60, 60, p);
        p.setColor(0xFF00FF00);
        p.setAntiAlias(true);
        c.drawCircle(32, 32, 10, p);
        int[] px = new int[W * H];
        bm.getPixels(px, 0, W, 0, 0, W, H);
        long t1 = System.nanoTime();
        int nonzero = 0, red = 0, green = 0;
        for (int v : px) {
            if ((v >>> 24) != 0) nonzero++;
            if (v == 0xFFFF0000) red++;
            if (v == 0xFF00FF00) green++;
        }
        System.out.println("DRAW-OK 64x64 nonzero=" + nonzero + " exactRed=" + red
                + " exactGreen=" + green
                + String.format(" px(0,0)=%08x px(4,4)=%08x px(32,32)=%08x", px[0], px[4 * W + 4], px[(H / 2) * W + W / 2]));
        System.out.println("draw+readback: " + String.format("%.2f", (t1 - t0) / 1e6) + " ms");

        long a = System.nanoTime();
        for (int i = 0; i < 1000; i++) c.drawRect(0, 0, 8, 8, p);
        long b = System.nanoTime();
        System.out.println("drawRect x1000: " + String.format("%.2f", (b - a) / 1e6) + " ms  ("
                + String.format("%.2f", (b - a) / 1e6) + " us/op scale)  "
                + String.format("%.1f", (b - a) / 1000.0 / 1000.0) + " us/op");

        long m0 = System.nanoTime();
        for (int i = 0; i < 1000; i++) { Bitmap t = Bitmap.createBitmap(W, H, Bitmap.Config.ARGB_8888); new Canvas(t); }
        long m1 = System.nanoTime();
        System.out.println("bitmap+canvas create: " + String.format("%.1f", (m1 - m0) / 1000.0 / 1000.0) + " us/op");

        try {
            RenderNode rn = new RenderNode("probe");
            rn.setPosition(0, 0, 100, 100);
            RecordingCanvas rc = rn.beginRecording();
            rc.drawRect(0, 0, 50, 50, p);
            rn.endRecording();
            System.out.println("RenderNode+RecordingCanvas: OK w=" + rn.getWidth() + " h=" + rn.getHeight());
        } catch (Throwable t) {
            System.out.println("RenderNode path: FAILED " + t);
        }
        System.out.println("RESULT done");
    }
}
