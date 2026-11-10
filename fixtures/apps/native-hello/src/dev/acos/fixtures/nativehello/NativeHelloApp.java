package dev.acos.fixtures.nativehello;

import android.app.Application;
import android.content.res.AssetManager;
import android.graphics.Bitmap;
import android.util.Log;

/**
 * Native fixture application: exercises an APK-shipped {@code libnativehello.so}
 * from {@code Application.onCreate}, so the evidence does not depend on the
 * Activity lifecycle reaching {@code onResume}.
 *
 * <p>It covers the three product seams:
 * <ul>
 *   <li>a plain native String return;</li>
 *   <li>an APK asset read through {@code AAssetManager};</li>
 *   <li>a {@code Bitmap} filled through {@code AndroidBitmap_lockPixels} and
 *       read back from Java with {@code Bitmap.getPixel}.</li>
 * </ul>
 */
public class NativeHelloApp extends Application {
    // Tagged `AcosFixture` so `acos launch` reports these lines in `events`
    // (the launch ladder's fixture-log channel).
    private static final String TAG = "AcosFixture";

    private native String nativeHello();

    private native String readAsset(AssetManager manager);

    private native int fillBitmap(Bitmap bitmap);

    @Override
    public void onCreate() {
        super.onCreate();
        // Loaded from onCreate, not a static initializer: the launch ladder
        // must not depend on the VM running `<clinit>` before `newInstance`.
        System.loadLibrary("nativehello");
        Log.i(TAG, "nativeHello=" + nativeHello());
        Log.i(TAG, "asset=" + readAsset(getAssets()));
        Bitmap bitmap = Bitmap.createBitmap(new int[4 * 3], 4, 3, Bitmap.Config.ARGB_8888);
        int pixels = fillBitmap(bitmap);
        Log.i(TAG, "bitmapPixels=" + pixels);
        Log.i(TAG, "bitmapPixel11=" + Integer.toHexString(bitmap.getPixel(1, 1)));
    }
}
