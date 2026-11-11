package dev.acos.fixtures.flutterhello;

import android.content.res.AssetManager;
import android.os.SystemClock;
import android.view.Surface;
import android.view.SurfaceControl;
import io.flutter.embedding.engine.FlutterJNI;
import io.flutter.embedding.engine.dart.PlatformMessageHandler;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
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

  /**
   * The reference device's density. {@code fixtures/reference-device/device.json}
   * records the moto g35 5G at 400 dpi, i.e. {@code 400/160 = 2.5}. W128-A used
   * 2.625 (420 dpi), which is the ART *emulator* density, not this phone: it
   * made the fixture's {@code physicalSize / dpr} background 411 px wide instead
   * of the reference's 432 and compounded the status-bar offset below.
   */
  private static final float DPR = 2.5f;

  /** Flutter's {@code PointerData} wire size (36 int64/float64 fields). */
  private static final int POINTER_FIELDS = 36;

  private static final int POINTER_BYTES = POINTER_FIELDS * 8;

  /** 60 Hz reference refresh, matching {@code fwk_vsync::DEFAULT_INTERVAL_NANOS}. */
  private static final long REFRESH_PERIOD_NANOS = 16_666_667L;

  /**
   * The reference device's status-bar height in physical pixels. Measured from
   * {@code fixtures/reference-device/flutter-hello/screenshot_initial.png}: the
   * opaque system band is rows {@code 0..107} (row 108 is already the app's
   * {@code 0xFF1B1B1F} background). Modelled as the viewport's
   * {@code physicalPaddingTop}; W128-A used 48dp * 2.625 = 126, 18 px too large.
   */
  private static final int STATUS_BAR_PX = 108;

  /**
   * Centre of the "counter: n" button in the Dart layout of {@code
   * flutter_probe_main.dart}.
   *
   * <p>That probe is {@code dart:ui}-only and does not apply a
   * {@code devicePixelRatio} transform, so its scene coordinates are the
   * physical pixel grid: {@code view.padding.top} is physical
   * ({@code STATUS_BAR_PX} = 108), the background is {@code physicalSize / dpr}
   * pixels, and the button is a 220x64 rect at (40, padding + 100). The Dart
   * pointer handler maps an incoming packet to {@code physical / dpr} before the
   * hit test, so the packet must carry {@code scene * dpr}. The W128-A tap was at
   * the screen centre (540, 132) logical, well outside the button — and even the
   * closest guess at the button centre used {@code top = 48dp} while the
   * rendered scene put it at 126px, so the up event missed every time.
   */
  private static final double BUTTON_CENTER_X = 40.0 + 220.0 / 2.0;

  private static final double BUTTON_CENTER_Y =
      STATUS_BAR_PX + 100.0 + 64.0 / 2.0;

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

  /**
   * W141-A semantics capture. The engine delivers the accessibility tree to
   * the embedder through {@code FlutterJNI.updateSemantics(buffer, strings,
   * attrs)}. {@code updateSemantics} is private in {@code FlutterJNI}, so the
   * delegate installed below is the supported interception point; the Rust
   * {@code FlutterSession} reads these fields and decodes the buffer with the
   * W137-A {@code decode_semantics_update} port.
   */
  static volatile byte[] sSemanticsBuffer;

  static volatile String[] sSemanticsStrings;

  static volatile int sSemanticsSeq;

  /** Whether semantics were requested (diagnostic). */
  private static volatile boolean sSemanticsEnabled;

  /** W156-A: the last exception thrown while capturing a semantics update. */
  static volatile String sSemanticsError;

  /**
   * W169-A platform-channel capture. The engine delivers a Dart
   * `sendPlatformMessage` to the embedder through the installed
   * {@link PlatformMessageHandler}; the raw probe answers the custom channel and
   * records which real channels (SystemChrome `flutter/platform`) it sees.
   */
  static volatile int sPlatformMessages;

  static volatile String sLastPlatformChannel = "";

  static volatile String sLastPlatformPayload = "";

  /** Responses from Dart to embedder->Dart dispatch (key data ack). */
  static volatile int sPlatformResponses;

  static volatile String sLastPlatformResponse = "";

  /** Diagnostic count of embedder->Dart dispatches. */
  static volatile int sDartInDispatches;

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
      jni.setAccessibilityDelegate(new SemanticsCapture());
      jni.setPlatformMessageHandler(new ProbeMessageHandler());
      System.out.println("ACOS_FLUTTER_PROBE phase=platformHandler ok");
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

      // The 27-argument native viewport contract (FlutterJNI 3.47):
      // devicePixelRatio, physicalSize, paddings, view insets, system-gesture
      // insets, touchSlop, the three display-feature rect arrays
      // (bounds/type/state), then minWidth, maxWidth, minHeight, maxHeight and
      // the four display-corner radii. Padding/insets/constraints are zero on
      // purpose: the fixture does not model insets, so the reference
      // comparison is stable. W128-A: the eighth trailing slot used to carry
      // `densutyDpi`, but this engine version reads that position as
      // `minWidth` — a non-zero value there constrains the view to 420 px.
      jni.setViewportMetrics(
          DPR,
          WIDTH,
          HEIGHT,
          STATUS_BAR_PX, // physicalPaddingTop (measured system status bar)
          0, // physicalPaddingRight
          0, // physicalPaddingBottom
          0, // physicalPaddingLeft
          0, // physicalViewInsetTop
          0, // physicalViewInsetRight
          0, // physicalViewInsetBottom
          0, // physicalViewInsetLeft
          0, // systemGestureInsetTop
          0, // systemGestureInsetRight
          0, // systemGestureInsetBottom
          0, // systemGestureInsetLeft
          0, // physicalTouchSlop
          new int[0], // displayFeaturesBounds
          new int[0], // displayFeaturesType
          new int[0], // displayFeaturesState
          0, // minWidth
          WIDTH, // maxWidth
          0, // minHeight
          HEIGHT, // maxHeight
          0, // physicalDisplayCornerRadiusTopLeft
          0, // physicalDisplayCornerRadiusTopRight
          0, // physicalDisplayCornerRadiusBottomRight
          0); // physicalDisplayCornerRadiusBottomLeft
      System.out.println("ACOS_FLUTTER_PROBE phase=viewport ok");

      jni.setRefreshRateFPS(60.0f);
      jni.updateRefreshRate();
      System.out.println("ACOS_FLUTTER_PROBE phase=refreshRate ok");

      jni.scheduleFrame();
      System.out.println("ACOS_FLUTTER_PROBE phase=scheduleFrame ok");

      // W141-A: the harness no longer taps on its own. The product
      // `FlutterSession` drives `tapAt` after the first frame has settled, so
      // the driver behaves like `acos tap` instead of hard-coding the
      // Increment button. `main` only schedules the first frame.
    } catch (Throwable t) {
      System.out.println("ACOS_FLUTTER_PROBE phase=error " + t);
      t.printStackTrace(System.out);
    }
  }

  /**
   * W169-A: the embedder side of the platform-message transport. Every channel
   * a real app uses (`flutter/platform`, `flutter/textinput`, `flutter/navigation`)
   * reaches this handler; the custom `acos/probe` channel is round-tripped so
   * the Dart reply callback is proven, not assumed.
   */
  private static final class ProbeMessageHandler implements PlatformMessageHandler {
    @Override
    public void handleMessageFromDart(
        String channel, ByteBuffer message, int replyId, long callbackHandle) {
      sPlatformMessages++;
      sLastPlatformChannel = channel;
      sLastPlatformPayload = decodeUtf8(message);
      System.out.println(
          "ACOS_FLUTTER_PROBE phase=platformMessage channel=" + channel
              + " replyId=" + replyId
              + " bytes=" + (message == null ? 0 : message.remaining())
              + " data=" + sLastPlatformPayload);
      if ("acos/probe".equals(channel)) {
        byte[] reply = "pong-from-java".getBytes(StandardCharsets.UTF_8);
        ByteBuffer buf = ByteBuffer.allocateDirect(reply.length);
        buf.put(reply);
        FlutterJNI jni = sJni;
        if (jni != null) {
          // A response to a Dart-originated message is delivered through
          // `invokePlatformMessageResponseCallback(replyId, ...)`, exactly as
          // `DartMessenger$Reply.reply` does; `dispatchPlatformMessage` would
          // be a brand-new embedder->Dart message instead.
          jni.invokePlatformMessageResponseCallback(replyId, buf, buf.position());
        }
        System.out.println(
            "ACOS_FLUTTER_PROBE phase=platformReply channel=" + channel
                + " replyId=" + replyId);
      } else if ("flutter/platform".equals(channel)) {
        System.out.println(
            "ACOS_FLUTTER_PROBE phase=systemChromeSeen channel=" + channel);
      }
    }

    @Override
    public void handlePlatformMessageResponse(int responseId, ByteBuffer message) {
      sPlatformResponses++;
      sLastPlatformResponse = decodeUtf8(message);
      System.out.println(
          "ACOS_FLUTTER_PROBE phase=platformResponse id=" + responseId
              + " bytes=" + (message == null ? 0 : message.remaining())
              + " data=" + sLastPlatformResponse);
    }
  }

  /** Read a ByteBuffer as UTF-8 without consuming the caller's position. */
  private static String decodeUtf8(ByteBuffer message) {
    if (message == null) {
      return "<null>";
    }
    ByteBuffer dup = message.duplicate();
    byte[] bytes = new byte[dup.remaining()];
    dup.get(bytes);
    return new String(bytes, StandardCharsets.UTF_8);
  }

  /**
   * W169-A harness driver: embedder -> Dart on a custom channel, which proves
   * the transport a real `MethodChannel` uses and exercises the Dart response
   * path (`handlePlatformMessageResponse`).
   */
  public static void sendDartIn() {
    FlutterJNI jni = sJni;
    if (jni == null) {
      return;
    }
    byte[] msg = "push-from-java".getBytes(StandardCharsets.UTF_8);
    ByteBuffer buf = ByteBuffer.allocateDirect(msg.length);
    buf.put(msg);
    jni.dispatchPlatformMessage("acos/dart_in", buf, buf.position(), 77);
    sDartInDispatches++;
    System.out.println("ACOS_FLUTTER_PROBE phase=dispatchDartIn bytes=" + msg.length);
  }

  /**
   * W169-A harness driver: a keyboard event on the engine's `flutter/keydata`
   * channel. The six little-endian uint64 fields are the `key_data_packet.h`
   * layout `PlatformDispatcher.onKeyData` unpacks (charDataSize, timeStamp,
   * type, physical, logical, synthesized) with no character payload.
   */
  public static void sendKeyData() {
    FlutterJNI jni = sJni;
    if (jni == null) {
      return;
    }
    ByteBuffer buf = ByteBuffer.allocateDirect(48);
    buf.order(ByteOrder.LITTLE_ENDIAN);
    buf.putLong(0L); // charDataSize
    buf.putLong(SystemClock.uptimeMillis() * 1000L); // timeStamp (micros)
    buf.putLong(0L); // KeyEventType.down
    buf.putLong(0L); // physical
    buf.putLong(0L); // logical
    buf.putLong(0L); // synthesized
    jni.dispatchPlatformMessage("flutter/keydata", buf, buf.position(), 88);
    sDartInDispatches++;
    System.out.println("ACOS_FLUTTER_PROBE phase=dispatchKeyData");
  }

  /**
   * W169-A harness driver: the Android back button. The embedding's
   * `FlutterActivityAndFragmentDelegate` sends `popRoute` on `flutter/navigation`
   * to Dart; a raw probe proves the engine carries it and records that no
   * framework handler exists to answer it.
   */
  public static void sendNavigationPopRoute() {
    FlutterJNI jni = sJni;
    if (jni == null) {
      return;
    }
    byte[] msg = new byte[] {0x01}; // placeholder raw method call
    ByteBuffer buf = ByteBuffer.allocateDirect(msg.length);
    buf.put(msg);
    jni.dispatchPlatformMessage("flutter/navigation", buf, buf.position(), 99);
    sDartInDispatches++;
    System.out.println("ACOS_FLUTTER_PROBE phase=dispatchNavigation");
  }

  /** W169-A diagnostics for the harness's machine-readable surface stage. */
  public static String platformDebug() {
    return "messages=" + sPlatformMessages
        + " lastChannel=" + sLastPlatformChannel
        + " lastPayload=" + sLastPlatformPayload
        + " responses=" + sPlatformResponses
        + " lastResponse=" + sLastPlatformResponse
        + " dispatches=" + sDartInDispatches;
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
    tapAt(BUTTON_CENTER_X * DPR, BUTTON_CENTER_Y * DPR);
  }

  /**
   * W141-A harness driver: dispatch a tap at physical {@code (x, y)} and
   * schedule a frame. The product `FlutterSession.tap` calls this.
   */
  public static void tapAt(double x, double y) {
    FlutterJNI jni = sJni;
    if (jni == null) {
      return;
    }
    ByteBuffer packet = buildTap(x, y);
    jni.dispatchPointerDataPacket(packet, packet.position());
    jni.scheduleFrame();
  }

  /** W141-A: enable the engine's accessibility (semantics) stream. */
  public static void enableSemantics() {
    FlutterJNI jni = sJni;
    if (jni == null) {
      return;
    }
    try {
      System.out.println(
          "ACOS_FLUTTER_PROBE phase=semanticsEnable begin attached=" + jni.isAttached());
      jni.setSemanticsEnabled(true);
      sSemanticsEnabled = true;
      System.out.println("ACOS_FLUTTER_PROBE phase=semanticsEnabled");
    } catch (Throwable t) {
      System.out.println("ACOS_FLUTTER_PROBE phase=semanticsEnableError " + t);
      t.printStackTrace(System.out);
    }
  }

  /**
   * W156-A: dispatch a semantics TAP on `id` through
   * `FlutterJNI.dispatchSemanticsAction` -> `nativeDispatchSemanticsAction`. The
   * fourth argument is the `AccessibilityBridge$Action.value` mask
   * (`TAP = 1`), which the engine maps onto `dart:ui`'s `SemanticsAction`.
   */
  public static void dispatchSemanticsTap(int id) {
    FlutterJNI jni = sJni;
    if (jni == null) {
      return;
    }
    // The app's action handler schedules the next frame; the driver must not
    // schedule one itself, or `FlutterSession::tap_text` cannot tell an action
    // that took effect from one that did not (and would skip its pointer
    // fallback).
    jni.dispatchSemanticsAction(id, 1, null, 0);
  }

  /** W156-A diagnostics: the engine's semantics sequence and buffer size. */
  public static int semanticsSeq() {
    return sSemanticsSeq;
  }

  public static boolean semanticsEnabled() {
    return sSemanticsEnabled;
  }

  public static int semanticsBufferSize() {
    return sSemanticsBuffer == null ? 0 : sSemanticsBuffer.length;
  }

  public static String semanticsDebug() {
    return "seq=" + sSemanticsSeq + " enabled=" + sSemanticsEnabled
        + " bytes=" + semanticsBufferSize()
        + " strings=" + (sSemanticsStrings == null ? 0 : sSemanticsStrings.length)
        + " err=" + sSemanticsError;
  }

  /**
   * W141-A: the embedder-side accessibility delegate. It copies the engine's
   * semantics update into static fields the Rust driver reads; the buffer is
   * decoded by the W137-A port, not here.
   */
  private static final class SemanticsCapture
      implements FlutterJNI.AccessibilityDelegate {
    @Override
    public void updateSemantics(
        ByteBuffer buffer, String[] strings, ByteBuffer[] attributeBuffers) {
      // Count the delivery first: a failure to copy the buffer must not hide
      // the fact that the engine reached the embedder at all.
      sSemanticsSeq++;
      try {
        ByteBuffer dup = buffer.duplicate();
        dup.order(ByteOrder.LITTLE_ENDIAN);
        dup.rewind();
        byte[] data = new byte[dup.remaining()];
        dup.get(data);
        sSemanticsBuffer = data;
        sSemanticsStrings = strings == null ? new String[0] : strings.clone();
      } catch (Throwable t) {
        sSemanticsError = String.valueOf(t);
        System.out.println("ACOS_FLUTTER_PROBE phase=semanticsError " + t);
      }
    }

    @Override
    public void updateCustomAccessibilityActions(ByteBuffer buffer, String[] strings) {}

    @Override
    public void setLocale(String locale) {}

    @Override
    public void resetSemantics() {
      sSemanticsBuffer = null;
      sSemanticsStrings = null;
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
    // Leave `position` at the end: it *is* the byte count the native side
    // copies out of the buffer's direct address (see the call sites).
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
