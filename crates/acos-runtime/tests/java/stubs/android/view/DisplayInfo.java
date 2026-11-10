package android.view;

/**
 * Signature-only stub for the {@code @hide} {@code android.view.DisplayInfo}.
 *
 * <p>Only used to type-check {@code DisplayMain} at build time; the runtime
 * resolves the real framework class from the boot classpath (parent-first), so
 * the real {@code CREATOR} reads the bytes.
 */
public class DisplayInfo {
    public int displayId;
    public int logicalWidth;
    public int logicalHeight;
    public int appWidth;
    public int appHeight;
    public String name;
    public int type;
    public int rotation;
    public int modeId;
    public float renderFrameRate;
    public int defaultModeId;
    public Display.Mode[] supportedModes;
    public int logicalDensityDpi;
    public int state;
    public int colorMode;
    public int[] supportedColorModes;
    public int installOrientation;
    public DisplayCutout displayCutout;
}
