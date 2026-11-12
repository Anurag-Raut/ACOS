package dev.acos.fixtures.hostileprobe;

import android.os.IBinder;
import android.os.Parcel;
import android.view.View;
import android.view.WindowManager;

/**
 * W44 hostile fixture: raw binder traffic a third-party APK can send.
 *
 * <p>All accesses to framework-internal classes are reflective, so this file
 * compiles against the public SDK like any app. A reflection failure is printed
 * as {@code ACOSHOSTILE {"probe":...,"error":...}} — it is a fixture limitation,
 * never evidence that the host refused anything.
 */
final class HostileParcels {
    private static final String LEGACY_COMPOSER = "android.ui.ISurfaceComposer";
    private static final int SET_TRANSACTION_STATE = 8;
    private static final String WINDOW_SESSION = "android.view.IWindowSession";
    private static final int ADD_TO_DISPLAY_WITHOUT_INPUT_CHANNEL = 3;

    private HostileParcels() {}

    private static void note(String json) {
        System.out.println("ACOSHOSTILE " + json);
        System.out.flush();
    }

    private static IBinder service(String name) {
        try {
            Class<?> sm = Class.forName("android.os.ServiceManager");
            java.lang.reflect.Method get = sm.getDeclaredMethod("getService", String.class);
            get.setAccessible(true);
            return (IBinder) get.invoke(null, name);
        } catch (Throwable t) {
            note("{\"probe\":\"service\",\"name\":\"" + name + "\",\"error\":\"" + escape(t) + "\"}");
            return null;
        }
    }

    private static String escape(Throwable t) {
        String s = t.getClass().getSimpleName() + ": " + t.getMessage();
        return s.replace("\\", "\\\\").replace("\"", "\\\"");
    }

    /** 257 composer states: over the host's MAX_COMPOSERS. */
    static void layerCountBomb() {
        IBinder sf = service("SurfaceFlinger");
        if (sf == null) return;
        Parcel data = Parcel.obtain();
        Parcel reply = Parcel.obtain();
        try {
            data.writeInterfaceToken(LEGACY_COMPOSER);
            data.writeInt(0);          // FrameTimelineInfo size word (empty)
            data.writeInt(257);        // composer_count > MAX_COMPOSERS (256)
            boolean rc = sf.transact(SET_TRANSACTION_STATE, data, reply, 0);
            note("{\"probe\":\"layer-count\",\"transact\":" + rc + "}");
        } catch (Throwable t) {
            note("{\"probe\":\"layer-count\",\"error\":\"" + escape(t) + "\"}");
        } finally {
            reply.recycle();
            data.recycle();
        }
    }

    /** One layer state whose GraphicBuffer declares an impossible geometry. */
    static void geometryBomb() {
        IBinder sf = service("SurfaceFlinger");
        if (sf == null) return;
        Parcel data = Parcel.obtain();
        Parcel reply = Parcel.obtain();
        try {
            data.writeInterfaceToken(LEGACY_COMPOSER);
            data.writeInt(0);          // FrameTimelineInfo size word (empty)
            data.writeInt(1);          // composer_count
            writeHostileLayer(data);
            boolean rc = sf.transact(SET_TRANSACTION_STATE, data, reply, 0);
            note("{\"probe\":\"geometry\",\"transact\":" + rc + "}");
        } catch (Throwable t) {
            note("{\"probe\":\"geometry\",\"error\":\"" + escape(t) + "\"}");
        } finally {
            reply.recycle();
            data.recycle();
        }
    }

    /**
     * The image's layer_state_t order, all zero except the buffer header. The
     * host must reject width/height/stride 0xffffffff before it reads a pixel.
     */
    private static void writeHostileLayer(Parcel d) {
        // A real binder object: libbinder writes a 4-byte null marker that the
        // legacy decoder would have to special-case; the fixture sends a normal
        // local binder so the hostile fields are what the host rejects.
        d.writeStrongBinder(new android.os.Binder()); // surface
        d.writeInt(1);             // layer_id
        d.writeLong(0L);           // what
        d.writeFloat(0f);          // x
        d.writeFloat(0f);          // y
        d.writeInt(0);             // z
        d.writeInt(0);             // layer_stack
        d.writeInt(0);             // flags
        d.writeInt(0);             // mask
        for (int i = 0; i < 4; i++) d.writeFloat(0f); // matrix
        for (int i = 0; i < 4; i++) d.writeInt(0); // ui::Rect crop (no presence word)
        d.writeInt(0);             // nullable SurfaceControl #1
        d.writeInt(0);             // nullable SurfaceControl #2
        for (int i = 0; i < 4; i++) d.writeFloat(0f); // color
        d.writeInt(0);             // window_info absent
        d.writeInt(0);             // transparent_region_size
        d.writeInt(0);             // buffer_transform
        d.writeInt(0);             // transform_to_display_inverse
        d.writeInt(0);             // border_enabled
        d.writeFloat(0f);          // border_width
        for (int i = 0; i < 4; i++) d.writeFloat(0f); // border colour
        d.writeInt(0);             // dataspace
        d.writeInt(0);             // hdr_size
        d.writeInt(0);             // damage_size
        d.writeInt(0);             // api
        d.writeInt(0);             // has sideband stream
        for (int i = 0; i < 16; i++) d.writeFloat(0f); // mat4
        d.writeFloat(0f);          // corner_radius
        d.writeInt(0);             // background_blur_radius
        d.writeInt(0);             // layer_metadata absent
        for (int i = 0; i < 4; i++) d.writeFloat(0f); // bg colour
        d.writeInt(0);             // bgColorDataspace
        d.writeInt(0);             // colorSpaceAgnostic
        d.writeInt(0);             // listener_count
        d.writeFloat(0f);          // shadow_radius
        d.writeInt(0);             // frame_rate_selection_priority
        d.writeFloat(0f);          // frame_rate
        d.writeInt(0);             // frame-rate byte 1
        d.writeInt(0);             // frame-rate byte 2
        d.writeInt(0);             // frame-rate byte 3
        d.writeInt(0);             // fixedTransformHint
        d.writeInt(0);             // autoRefresh
        d.writeInt(0);             // dimmingEnabled
        d.writeInt(0);             // blur_region_count
        for (int i = 0; i < 10; i++) d.writeFloat(0f); // StretchEffect
        for (int i = 0; i < 4; i++) d.writeInt(0); // buffer_crop Rect
        for (int i = 0; i < 4; i++) d.writeInt(0); // destination_frame Rect
        d.writeInt(0);             // is_trusted_overlay
        d.writeInt(0);             // drop_input_mode
        d.writeInt(1);             // has_buffer_data
        d.writeInt(1);             // BufferData present marker
        d.writeInt(0);             // BufferData.flags
        d.writeInt(1);             // has GraphicBuffer
        d.writeInt(52);            // header len: 13 words
        d.writeInt(0);             // fd count: geometry is rejected first
        d.writeInt(0x31304247);    // 'GB01' in the image's byte order
        d.writeInt(0xffffffff);    // width
        d.writeInt(0xffffffff);    // height
        d.writeInt(0xffffffff);    // stride
        d.writeInt(1);             // format RGBA_8888
        for (int i = 0; i < 8; i++) d.writeInt(0);
    }

    /** `out float[]` length i32::MAX on the real window session. */
    static void sizeCompatScaleBomb(View decor) {
        try {
            Object session = windowSession(decor);
            Object window = windowToken(decor);
            if (session == null || window == null) {
                note("{\"probe\":\"size-compat\",\"error\":\"no live window session\"}");
                return;
            }
            IBinder sessionBinder = (IBinder) session.getClass()
                    .getMethod("asBinder").invoke(session);
            IBinder windowBinder = (IBinder) window.getClass()
                    .getMethod("asBinder").invoke(window);
            WindowManager.LayoutParams lp = (WindowManager.LayoutParams) decor
                    .getLayoutParams();
            Parcel data = Parcel.obtain();
            Parcel reply = Parcel.obtain();
            try {
                data.writeInterfaceToken(WINDOW_SESSION);
                data.writeStrongBinder(windowBinder);
                data.writeInt(1);
                lp.writeToParcel(data, 0);
                data.writeInt(0);                 // viewVisibility
                data.writeInt(0);                 // layerStackId
                data.writeInt(Integer.MAX_VALUE); // out float[] length
                boolean rc = sessionBinder.transact(
                        ADD_TO_DISPLAY_WITHOUT_INPUT_CHANNEL, data, reply, 0);
                note("{\"probe\":\"size-compat\",\"transact\":" + rc + "}");
            } finally {
                reply.recycle();
                data.recycle();
            }
        } catch (Throwable t) {
            note("{\"probe\":\"size-compat\",\"error\":\"" + escape(t) + "\"}");
        }
    }

    /**
     * The live `ViewRootImpl`: the DecorView's parent. (`WindowManagerGlobal`'s
     * `mViews` list is not reachable here: the reflection is refused and the
     * field read yields the DecorView itself in this runtime.)
     */
    private static Object viewRoot(View decor) {
        android.view.ViewParent parent = decor.getParent();
        if (parent == null) return null;
        return parent;
    }

    private static Object windowSession(View decor) throws Exception {
        Object viewRoot = viewRoot(decor);
        if (viewRoot == null) return null;
        java.lang.reflect.Field field = viewRoot.getClass().getDeclaredField("mWindowSession");
        field.setAccessible(true);
        return field.get(viewRoot);
    }

    private static Object windowToken(View decor) throws Exception {
        Object viewRoot = viewRoot(decor);
        if (viewRoot == null) return null;
        java.lang.reflect.Field field = viewRoot.getClass().getDeclaredField("mWindow");
        field.setAccessible(true);
        return field.get(viewRoot);
    }
}
