import android.hardware.OverlayProperties;
import android.hardware.display.IDisplayManager;
import android.os.IBinder;
import android.os.ServiceManager;
import android.view.Display;
import android.view.DisplayInfo;

/**
 * Acceptance for the {@code IDisplayManager} replies: the real framework
 * {@code $Stub$Proxy} reads the {@code DisplayInfo} / {@code OverlayProperties}
 * that {@code acos-runtime::display_java} materialises and serialises.
 *
 * <p>{@code acos-runtime} builds the real Java objects and lets their own
 * {@code writeToParcel} emit the bytes, so the values asserted here come out of
 * the real {@code DisplayInfo.CREATOR} / {@code OverlayProperties.CREATOR}
 * after a full transact round trip. The reference geometry is the committed
 * {@code fixtures/reference/hello-views/art-api34-x86_64/display.txt}.
 */
public final class DisplayMain {
    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    static void out(String key, Object value) {
        System.out.println(key + "=" + value);
    }

    public static void main(String[] args) throws Exception {
        IBinder binder = ServiceManager.getService("display");
        check(binder != null, "getService(display) must return a binder");
        IDisplayManager dm = IDisplayManager.Stub.asInterface(binder);
        check(dm != null, "asInterface(display) must produce a proxy");

        DisplayInfo info = dm.getDisplayInfo(0);
        check(info != null, "getDisplayInfo(0) must not be null");
        out("displayId", info.displayId);
        out("name", info.name);
        out("type", info.type);
        out("logical", info.logicalWidth + "x" + info.logicalHeight);
        out("app", info.appWidth + "x" + info.appHeight);
        out("rotation", info.rotation);
        out("densityDpi", info.logicalDensityDpi);
        out("state", info.state);
        out("colorMode", info.colorMode);
        out("modeId", info.modeId);
        out("defaultModeId", info.defaultModeId);
        out("installOrientation", info.installOrientation);

        check(info.displayId == 0, "displayId");
        check("Built-in Screen".equals(info.name), "name: " + info.name);
        check(info.type == 1, "type must be TYPE_INTERNAL(1), got " + info.type);
        check(info.logicalWidth == 1080 && info.logicalHeight == 2400, "logical bounds");
        check(info.appWidth == 1080 && info.appHeight == 2209, "app bounds");
        check(info.rotation == 0, "rotation must be ROTATION_0");
        check(info.logicalDensityDpi == 420, "density must be 420dpi");
        check(info.state == 2, "state must be STATE_ON(2)");
        check(info.colorMode == 0, "colorMode must be COLOR_MODE_SRGB(0)");
        check(info.modeId == 1 && info.defaultModeId == 1, "active/default mode id");
        check(Math.abs(info.renderFrameRate - 60.000004f) < 0.001f,
                "renderFrameRate: " + info.renderFrameRate);
        check(info.installOrientation == 0, "installOrientation");

        // The active mode survived the real `Display$Mode.writeToParcel`.
        check(info.supportedModes != null && info.supportedModes.length == 1,
                "supportedModes length");
        Display.Mode mode = info.supportedModes[0];
        out("mode", mode.getModeId() + "/" + mode.getPhysicalWidth() + "x"
                + mode.getPhysicalHeight() + "@" + mode.getRefreshRate());
        check(mode.getModeId() == 1, "mode id");
        check(mode.getPhysicalWidth() == 1080 && mode.getPhysicalHeight() == 2400,
                "mode dimensions");
        check(Math.abs(mode.getRefreshRate() - 60.000004f) < 0.001f, "mode refresh rate");
        check(info.supportedColorModes != null && info.supportedColorModes.length == 1
                && info.supportedColorModes[0] == 0, "supportedColorModes = {SRGB}");

        // The cutout went through `DisplayCutout$ParcelableWrapper`.
        check(info.displayCutout != null, "displayCutout must not be null");
        check(info.displayCutout.getSafeInsetTop() == 0
                && info.displayCutout.getSafeInsetLeft() == 0
                && info.displayCutout.getSafeInsetRight() == 0
                && info.displayCutout.getSafeInsetBottom() == 0,
                "NO_CUTOUT safe insets must all be zero");
        // `DisplayCutout.NO_CUTOUT` is a hidden field, so identify it by the
        // all-zero safe insets it round-tripped with.
        out("cutout", "no-safe-insets");

        // `getOverlaySupport` round-trips the native overlay properties object.
        OverlayProperties props = dm.getOverlaySupport();
        check(props != null, "getOverlaySupport must not be null");
        check(!props.supportFp16ForHdr(), "overlay fp16 must be false");
        check(!props.supportMixedColorSpaces(), "overlay mixed color spaces must be false");
        out("overlay", "fp16=false mixedColorSpaces=false");

        // Unknown display ids desynchronise nothing: the next call is clean.
        check(dm.getDisplayInfo(7) == null, "getDisplayInfo(7) must be null");
        DisplayInfo again = dm.getDisplayInfo(0);
        check(again != null && again.logicalWidth == 1080,
                "getDisplayInfo(0) after a null reply");
        out("missingDisplay", "null");

        System.out.println("display-ok");
    }
}
