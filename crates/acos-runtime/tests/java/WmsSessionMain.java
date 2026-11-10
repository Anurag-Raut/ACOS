package android.view;

import android.os.Bundle;
import android.os.IBinder;
import android.os.ServiceManager;
import android.util.MergedConfiguration;
import android.window.ClientWindowFrames;

/**
 * W20-A deferred acceptance: the real {@code IWindowSession$Stub$Proxy} reads
 * the ACOS WMS {@code addToDisplayAsUser} / {@code relayout} replies over the
 * real Binder transport.
 *
 * <p>The reply carries a real {@code InputChannel} (read through
 * {@code nativeReadFromParcel}) and a real {@code SurfaceControl}
 * ({@code nativeReadFromParcel}); {@code Surface.copyFrom(SurfaceControl)} must
 * then materialise a valid surface. Incoming {@code IWindow} callbacks are
 * delivered oneway during these calls and are counted by the stub.
 *
 * <p>Package {@code android.view} so the framework-package-private
 * {@code SurfaceControl()} constructor is reachable, exactly as
 * {@code ViewRootImpl} uses it.
 */
public final class WmsSessionMain {
    static int visibilityCalls = 0;
    static int focusCalls = 0;
    static int resizedCalls = 0;
    static int insetsCalls = 0;

    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    static void out(String key, Object value) {
        System.out.println(key + "=" + value);
    }

    public static void main(String[] args) throws Exception {
        IBinder binder = ServiceManager.getService("window");
        check(binder != null, "getService(window)");
        IWindowManager wm = IWindowManager.Stub.asInterface(binder);
        IWindowSession session = wm.openSession(null);
        check(session != null, "openSession");

        IWindow window = new IWindow.Stub() {
            @Override
            public void dispatchAppVisibility(boolean visible) {
                visibilityCalls++;
            }

            @Override
            public void windowFocusChanged(boolean focused) {
                focusCalls++;
            }

            @Override
            public void resized(ClientWindowFrames frames, boolean reportDraw,
                    MergedConfiguration mergedConfiguration, InsetsState insetsState,
                    boolean forceLayout, boolean alwaysConsumeSystemBars, int displayId,
                    int syncSeqId, boolean dragResizing) {
                resizedCalls++;
            }

            @Override
            public void insetsControlChanged(InsetsState insetsState,
                    InsetsSourceControl[] activeControls) {
                insetsCalls++;
            }
        };

        WindowManager.LayoutParams attrs = new WindowManager.LayoutParams();
        attrs.packageName = "dev.acos.fixtures.helloviews";
        attrs.setTitle("MainActivity");

        InputChannel channel = new InputChannel();
        InsetsState addInsets = new InsetsState();
        InsetsSourceControl.Array addControls = new InsetsSourceControl.Array();
        android.graphics.Rect addFrame = new android.graphics.Rect();
        float[] addFloat = new float[1];
        int addResult = session.addToDisplayAsUser(window, attrs, 0, 0, 0, 0,
                channel, addInsets, addControls, addFrame, addFloat);
        out("addResult", addResult);
        out("channelName", channel.getName());
        check(channel.getName() != null, "channel has a name");
        check(channel.getToken() != null, "channel has a token");

        ClientWindowFrames frames = new ClientWindowFrames();
        MergedConfiguration merged = new MergedConfiguration();
        SurfaceControl sc = new SurfaceControl();
        InsetsState insets = new InsetsState();
        InsetsSourceControl.Array controls = new InsetsSourceControl.Array();
        Bundle bundle = new Bundle();
        int relayoutResult = session.relayout(window, attrs, 1080, 2209, 0, 0, 1, 1,
                frames, merged, sc, insets, controls, bundle);
        out("relayoutResult", relayoutResult);
        out("frame", frames.frame);
        out("displayFrame", frames.displayFrame);
        out("surfaceValid", sc.isValid());

        Surface surface = new Surface();
        surface.copyFrom(sc);
        out("surfaceIsValid", surface.isValid());
        surface.release();

        out("visibilityCalls", visibilityCalls);
        out("focusCalls", focusCalls);
        out("resizedCalls", resizedCalls);
        out("insetsCalls", insetsCalls);

        sc.release();
        channel.dispose();
        session.remove(window);
        System.out.println("wmssession=ok");
    }
}
