package dev.acos.fixtures.flutterhello;

import android.content.res.AssetManager;
import android.os.SystemClock;
import android.view.Surface;
import android.view.SurfaceControl;
import io.flutter.embedding.engine.FlutterJNI;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.ArrayList;
import java.util.List;

/**
 * ACOS W99-A: Java harness that drives the real Flutter embedding entry points
 * (`FlutterJNI`) exactly the way `FlutterShellArgs`/`FlutterEngine` do, so the
 * engine's registered natives run from real Java inside the ACOS VM.
 *
 * <p>W93-A proved the engine maps, {@code init}/{@code attach}/{@code runBundle}
 * succeed and Dart {@code main()} runs. W99-A adds the three things a first
 * frame needs:
 *
 * <ul>
 *   <li>a real {@link AssetManager} over the fixture APK (so {@code
 *       AAssetManager_fromJava} resolves and {@code flutter_assets} load);
 *   <li>a real {@link Surface} built from a {@link SurfaceControl} layer (the
 *       engine's software renderer presents through {@code ANativeWindow_lock}/
 *       {@code unlockAndPost});
 *   <li>viewport metrics, a scheduled frame, and an engine-dispatched pointer
 *       packet (a tap on the counter) through {@code
 *       dispatchPointerDataPacket}.
 * </ul>
 *
 * <p>It uses only the public embedding surface (no reflection). The fixture is
 * AOT, so {@code bundlePath} is the {@code libapp.so} path the loader extracted
 * from the APK.
 */
public final class FlutterProbe {
  private FlutterProbe() {}

  /** Reference device geometry (fixtures/reference-device/flutter-hello). */
  private static final int WIDTH = 1080;

  private static final int HEIGHT = 2400;
  private static final float DPR = 2.625f;
  private static final int DENSITY_DPI = 420;

  /** Flutter's {@code PointerData} wire size (36 int64/float64 fields). */
  private static final int POINTER_FIELDS = 36;

  private static final int POINTER_BYTES = POINTER_FIELDS * 8;

  /** 60 Hz reference refresh, matching {@code fwk_vsync::DEFAULT_INTERVAL_NANOS}. */
  private static final long REFRESH_PERIOD_NANOS = 16_666_667L;

  /**
   * The live engine handle. The engine's vsync path is native -> Java
   * ({@code FlutterJNI.asyncWaitForVsync}) -> delegate; the delegate only
   * records the request, and the Rust harness delivers the matching
   * {@code onVsync} on the VM owner (platform) thread.
   */
  private static FlutterJNI sJni;

  /** Cookie of the most recent {@code asyncWaitForVsync} request, or -1. */
  private static volatile long sPendingCookie = -1L;

  /** Number of {@code asyncWaitForVsync} requests seen (diagnostic). */
  private static volatile long sRequestCount = 0L;

  public static void main(String[] args) {
    String bundlePath = args.length > 0 ? args[0] : null;
    String entrypoint = args.length > 1 ? args[1] : "main";
    String assetsDir = args.length > 2 ? args[2] : "";
    String apkPath = args.length > 3 ? args[3] : null;
    System.out.println("ACOS_FLUTTER_PROBE phase=start bundle=" + bundlePath);
    try {
      System.out.println("ACOS_FLUTTER_PROBE phase=loadLibrary begin");
      System.loadLibrary("flutter");
      System.out.println("ACOS_FLUTTER_PROBE phase=loadLibrary ok");

      AssetManager assets = new AssetManager();
      int cookie = assets.addAssetPath(apkPath);
      System.out.println(
          "ACOS_FLUTTER_PROBE phase=assets cookie=" + cookie + " apk=" + apkPath);

      FlutterJNI jni = new FlutterJNI();
      sJni = jni;
      installVsyncBridge(jni);
      System.out.println("ACOS_FLUTTER_PROBE phase=constructed jni=" + jni);


      // The Flutter 3.47 engine defaults to Impeller, which does not support
      // software rendering: `flutter_main.cc` fatal-checks
      // `!settings.enable_impeller` when `--enable-software-rendering` is set.
      // For an AOT build the engine also needs the shared-library name the
      // embedding's `FlutterLoader` adds (`--aot-shared-library-name=<path>`).
      String[] shellArgs =
          new String[] {
            "--enable-software-rendering",
            "--enable-impeller=false",
            "--verbose-logging",
            "--aot-shared-library-name=" + bundlePath
          };
      jni.init(null, shellArgs, null, null, null, System.currentTimeMillis(), 0);
      System.out.println("ACOS_FLUTTER_PROBE phase=init ok");

      jni.attachToNative();
      System.out.println("ACOS_FLUTTER_PROBE phase=attach ok");

      // `bundlePath` is NOT the snapshot: the embedding passes
      // `FlutterApplicationInfo.flutterAssetsDir` ("flutter_assets"), i.e. the
      // directory *inside the APK assets* that holds AssetManifest.bin /
      // FontManifest.json. The AOT snapshot comes from
      // `--aot-shared-library-name` above. Passing libapp.so here made the
      // engine look for `libapp.so/FontManifest.json` via AAssetManager.
      String flutterAssetsDir = "flutter_assets";
      List<String> dartArgs = new ArrayList<String>();
      jni.runBundleAndSnapshotFromLibrary(
          flutterAssetsDir, entrypoint, null, assets, dartArgs, 0L);
      System.out.println(
          "ACOS_FLUTTER_PROBE phase=runBundle ok assetsDir=" + flutterAssetsDir);

      // A real Surface over a real SurfaceControl layer. `ANativeWindow_fromSurface`
      // in the engine resolves `Surface.mNativeObject` -> layer -> geometry.
      SurfaceControl control =
          new SurfaceControl.Builder()
              .setName("acos-flutter-hello")
              .setBufferSize(WIDTH, HEIGHT)
              .build();
      System.out.println("ACOS_FLUTTER_PROBE phase=surfaceControl valid=" + control.isValid());
      Surface surface = new Surface(control);
      System.out.println("ACOS_FLUTTER_PROBE phase=surface valid=" + surface.isValid());

      jni.onSurfaceCreated(surface);
      System.out.println("ACOS_FLUTTER_PROBE phase=surfaceCreated ok");

      // The 27-argument native viewport contract: devicePixelRatio, physical
      // size, paddings, view insets, system-gesture insets, touch slop, three
      // rect arrays (gesture exclusion / display features / cutout) and the
      // trailing display metrics. Padding is zero: the fixture deliberately
      // does not model insets so the reference comparison is stable.
      jni.setViewportMetrics(
          DPR,
          WIDTH,
          HEIGHT,
          0,
          0,
          0,
          0,
          0,
          0,
          0,
          0,
          0,
          0,
          0,
          0,
          0,
          new int[0],
          new int[0],
          new int[0],
          DENSITY_DPI,
          0,
          0,
          0,
          0,
          0,
          0,
          0);
      System.out.println("ACOS_FLUTTER_PROBE phase=viewport ok");

      jni.setRefreshRateFPS(60.0f);
      jni.updateRefreshRate();
      System.out.println("ACOS_FLUTTER_PROBE phase=refreshRate ok");

      jni.scheduleFrame();
      System.out.println("ACOS_FLUTTER_PROBE phase=scheduleFrame ok");

      // The task runner is the caller's ALooper (pumped by the Rust harness),
      // so everything above is queued in order; the tap below follows the
      // surface/viewport tasks.
      ByteBuffer packet = buildTap(WIDTH * DPR / 2, (100 + 32) * DPR);
      jni.dispatchPointerDataPacket(packet, 0);
      System.out.println("ACOS_FLUTTER_PROBE phase=pointerDispatched bytes=" + packet.capacity());
      jni.scheduleFrame();
      System.out.println("ACOS_FLUTTER_PROBE phase=tapScheduled");
    } catch (Throwable t) {
      System.out.println("ACOS_FLUTTER_PROBE phase=error " + t);
      t.printStackTrace(System.out);
    }
  }

  /**
   * Installs the embedder-side vsync bridge. The engine calls the private
   * static {@code FlutterJNI.asyncWaitForVsync(cookie)} from native; the
   * delegate records the cookie and returns without re-entering the engine
   * (calling {@code onVsync} inline from the requesting thread faults the
   * process, and calling it from a non-VM thread violates the JNI contract).
   * The Rust harness then delivers the matching {@code nativeOnVsync} at a
   * monotonic 60 Hz cadence on the platform (VM owner) thread, the same pull
   * model as {@code fwk_vsync}.
   */
  private static void installVsyncBridge(FlutterJNI jni) {
    jni.setAsyncWaitForVsyncDelegate(
        new FlutterJNI.AsyncWaitForVsyncDelegate() {
          @Override
          public void asyncWaitForVsync(long cookie) {
            sPendingCookie = cookie;
            sRequestCount++;
            System.out.println("ACOS_FLUTTER_PROBE phase=vsyncRequest cookie=" + cookie);
          }
        });
  }

  /**
   * Called by the Rust harness on the VM owner (platform) thread once per
   * 60 Hz tick. Delivers {@code FlutterJNI.onVsync} for the outstanding
   * request only; returns the cookie, or -1 when the engine has not asked.
   *
   * @param frameDelayNanos nanoseconds between the synthetic vsync and now
   */
  /** Number of engine vsync requests observed (diagnostic). */
  /**
   * Harness driver: request another frame from the platform thread.
   *
   * <p>A real app is driven by its own animation callbacks; the probe's single
   * {@code scheduleFrame()} runs while the <em>first</em> frame is still in
   * flight (the animator coalesces it), so the harness asks again once that
   * frame has settled and the rasterizer has a surface.
   */
  public static void requestFrame() {
    FlutterJNI jni = sJni;
    if (jni != null) {
      jni.scheduleFrame();
    }
  }

  /** Harness driver: re-dispatch the counter tap (down+up) and schedule. */
  public static void tapIncrement() {
    FlutterJNI jni = sJni;
    if (jni != null) {
      ByteBuffer packet = buildTap(WIDTH * DPR / 2, (100 + 32) * DPR);
      jni.dispatchPointerDataPacket(packet, 0);
      jni.scheduleFrame();
    }
  }

  public static long vsyncRequestCount() {
    return sRequestCount;
  }

  public static long deliverVsync(long frameDelayNanos) {
    long cookie = sPendingCookie;
    if (cookie < 0L) {
      return -1L;
    }
    sPendingCookie = -1L;
    sJni.onVsync(frameDelayNanos, REFRESH_PERIOD_NANOS, cookie);
    return cookie;
  }

  /**
   * Build a Flutter pointer packet: one {@code PointerChange.down} followed by
   * one {@code up} at the same position inside the counter button.
   *
   * <p>The wire layout is the {@code PointerDataPacket::PointerData::Encode}
   * contract (36 little-endian 8-byte fields): embedderId, timeStamp, change,
   * kind, signalKind, device, pointerIdentifier, physicalX/Y, physicalDeltaX/Y,
   * buttons, obscured, synthesized, pressure/min/max, distance/max, size,
   * radiusMajor/Minor/Min/Max, orientation, tilt, platformData, scrollDeltaX/Y,
   * panX/Y, panDeltaX/Y, scale, rotation, viewId.
   */
  private static ByteBuffer buildTap(double x, double y) {
    ByteBuffer buffer = ByteBuffer.allocateDirect(POINTER_BYTES * 2);
    buffer.order(ByteOrder.LITTLE_ENDIAN);
    long now = SystemClock.uptimeMillis() * 1000L;
    // PointerChange.down = 4, PointerDeviceKind.touch = 0.
    encodePointer(buffer, now, 4, x, y);
    // PointerChange.up = 6.
    encodePointer(buffer, now + 50_000L, 6, x, y);
    buffer.position(0);
    return buffer;
  }

  private static void encodePointer(
      ByteBuffer buffer, long timestampMicros, int change, double x, double y) {
    buffer.putLong(0L); // embedderId
    buffer.putLong(timestampMicros); // timeStamp
    buffer.putLong(change); // change
    buffer.putLong(0L); // kind = touch
    buffer.putLong(0L); // signalKind = none
    buffer.putLong(0L); // device
    buffer.putLong(1L); // pointerIdentifier
    buffer.putDouble(x); // physicalX
    buffer.putDouble(y); // physicalY
    buffer.putDouble(0.0); // physicalDeltaX
    buffer.putDouble(0.0); // physicalDeltaY
    buffer.putLong(1L); // buttons (kPrimaryButton)
    buffer.putLong(0L); // obscured
    buffer.putLong(0L); // synthesized
    buffer.putDouble(1.0); // pressure
    buffer.putDouble(0.0); // pressureMin
    buffer.putDouble(1.0); // pressureMax
    buffer.putDouble(0.0); // distance
    buffer.putDouble(0.0); // distanceMax
    buffer.putDouble(0.0); // size
    buffer.putDouble(0.0); // radiusMajor
    buffer.putDouble(0.0); // radiusMinor
    buffer.putDouble(0.0); // radiusMin
    buffer.putDouble(0.0); // radiusMax
    buffer.putDouble(0.0); // orientation
    buffer.putDouble(0.0); // tilt
    buffer.putLong(0L); // platformData
    buffer.putDouble(0.0); // scrollDeltaX
    buffer.putDouble(0.0); // scrollDeltaY
    buffer.putDouble(0.0); // panX
    buffer.putDouble(0.0); // panY
    buffer.putDouble(0.0); // panDeltaX
    buffer.putDouble(0.0); // panDeltaY
    buffer.putDouble(1.0); // scale
    buffer.putDouble(0.0); // rotation
    buffer.putLong(0L); // viewId
  }
}
