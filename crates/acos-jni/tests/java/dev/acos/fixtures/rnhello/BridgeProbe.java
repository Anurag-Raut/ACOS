// W88-A: harness Java entry that drives the *real* React Native legacy (Paper)
// bridge inside the ACOS VM, so the Hermes bundle runs past
// `__fbBatchedBridgeConfig` and issues its first UIManager batch.
//
// This is the W84-A `HermesProbe` approach extended from "call the merged
// natives" to "construct real RN bridge objects":
//
//   1. load the fixture `.so`s + merged `OpenSourceMergedSoMapping` onloads,
//   2. build a minimal native-module registry (real `UIManagerModule`,
//      `AndroidInfoModule` (PlatformConstants), `DeviceInfoModule`,
//      `ExceptionsManagerModule`, `TimingModule`, `AppStateModule`) with our
//      real view managers,
//   3. construct `CatalystInstanceImpl` (its private ctor calls native
//      `initializeBridge`, which installs the JSI bridge globals),
//   4. `runJSBundle()`, then `AppRegistry.runApplication` to mount the tree,
//   5. emit every JS-originated UIManager op as `ACOS_RN_OP {...}`.
//
// Every stage prints an `ACOS_RN_BRIDGE {...}` (or op) line so the first
// failing stage survives a crash and the Rust test can parse/assert it.

package dev.acos.fixtures.rnhello;

import android.content.Context;
import android.content.ContextWrapper;
import android.content.pm.ApplicationInfo;
import android.content.res.AssetManager;
import android.content.res.Resources;
import android.os.Build;
import android.os.Bundle;
import android.os.IBinder;
import android.os.Looper;
import android.util.DisplayMetrics;
import android.view.MotionEvent;

import com.facebook.hermes.reactexecutor.HermesExecutorFactory;
import com.facebook.react.internal.AndroidChoreographerProvider;
import com.facebook.react.internal.ChoreographerProvider;
import com.facebook.react.modules.core.ReactChoreographer;
import com.facebook.react.NativeModuleRegistryBuilder;
import com.facebook.react.ReactPackage;
import com.facebook.react.bridge.Arguments;
import com.facebook.react.bridge.BaseJavaModule;
import com.facebook.react.bridge.BridgeReactContext;
import com.facebook.react.bridge.CatalystInstanceImpl;
import com.facebook.react.bridge.DefaultJSExceptionHandler;
import com.facebook.react.bridge.JSBundleLoader;
import com.facebook.react.bridge.JavaScriptExecutor;
import com.facebook.react.bridge.ModuleHolder;
import com.facebook.react.bridge.NativeModule;
import com.facebook.react.bridge.NativeModuleRegistry;
import com.facebook.react.bridge.ReactMethod;
import com.facebook.react.bridge.ReadableArray;
import com.facebook.react.bridge.ReadableType;
import com.facebook.react.bridge.ReadableMap;
import com.facebook.react.bridge.WritableMap;
import com.facebook.react.bridge.queue.MessageQueueThreadSpec;
import com.facebook.react.bridge.queue.ReactQueueConfigurationSpec;
import com.facebook.react.modules.appstate.AppStateModule;
import com.facebook.react.modules.core.TimingModule;
import com.facebook.react.devsupport.ReleaseDevSupportManager;
import com.facebook.react.modules.deviceinfo.DeviceInfoModule;
import com.facebook.react.modules.systeminfo.AndroidInfoModule;
import com.facebook.react.soloader.OpenSourceMergedSoMapping;
import com.facebook.soloader.SoLoader;
import com.facebook.soloader.SoSource;
import com.facebook.react.views.text.ReactRawTextManager;
import com.facebook.react.views.text.ReactTextViewManager;
import com.facebook.react.views.text.ReactVirtualTextViewManager;
import com.facebook.react.views.view.ReactViewManager;
import com.facebook.react.uimanager.DisplayMetricsHolder;
import com.facebook.react.uimanager.UIManagerModule;
import com.facebook.react.uimanager.ViewManager;
import com.facebook.react.uimanager.events.EventDispatcher;
import com.facebook.react.uimanager.events.TouchEvent;
import com.facebook.react.uimanager.events.TouchEventCoalescingKeyHelper;
import com.facebook.react.uimanager.events.TouchEventType;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

public final class BridgeProbe {

  public static Object PROBE_STATIC;
  public static Object[] PROBE_ARR;
  public static volatile Object PROBE_VOL;

  /**
   * W158-A: the live recording UIManager, so the product `RnSession` can read
   * the op stream and dispatch a tap after the driver `main` has returned.
   */
  static LoggingUIManagerModule sUiManager;

  /** The recorded ops as a JSON array (each element is already an op object). */
  public static String opsJson() {
    if (sUiManager == null) {
      return "[]";
    }
    List<String> ops = sUiManager.ops();
    StringBuilder sb = new StringBuilder("[");
    for (int i = 0; i < ops.size(); i++) {
      if (i > 0) {
        sb.append(',');
      }
      sb.append(ops.get(i));
    }
    return sb.append(']').toString();
  }

  /**
   * W158-A: dispatch a synthetic touch on `tag` through the real RN event path
   * (`UIManagerModule.getEventDispatcher()` + `TouchEvent`), exactly as W157-A
   * proved. Used by the product tap after the mount drain has returned.
   */
  public static void dispatchTapForTag(int tag) {
    if (sUiManager == null || tag <= 0) {
      stage("tap", "no-target-tag");
      return;
    }
    try {
      EventDispatcher ed = sUiManager.getEventDispatcher();
      TouchEventCoalescingKeyHelper helper = new TouchEventCoalescingKeyHelper();
      long t = android.os.SystemClock.uptimeMillis();
      float x = 8f;
      float y = 8f;
      MotionEvent down = MotionEvent.obtain(t, t, MotionEvent.ACTION_DOWN, x, y, 0);
      ed.dispatchEvent(TouchEvent.obtain(tag, TouchEventType.START, down, t, x, y, helper));
      MotionEvent up = MotionEvent.obtain(t, t + 40, MotionEvent.ACTION_UP, x, y, 0);
      ed.dispatchEvent(TouchEvent.obtain(tag, TouchEventType.END, up, t, x, y, helper));
      System.out.println("ACOS_RN_TAP_START {\"tag\":" + tag + "}");
      System.out.flush();
      stage("tap", "dispatched tag=" + tag);
    } catch (Throwable t) {
      emit("{\"phase\":\"tap-error\",\"error\":\"" + esc(t.toString()) + "\"}");
      dump(t);
    }
  }

  /**
   * W158-A: post a tap on `tag`, then re-enter `Looper.loop()` with a bounded
   * quit. Called through the VM's `push_static_call` + scheduler so the
   * asynchronous RN work (JS responder, Choreographer flush) drains exactly as
   * it did for the W157-A mount. The main looper's queue is `quitAllowed=false`,
   * so `Looper.quit()` throws before it can destroy the queue — the loop is
   * safely re-enterable.
   */
  public static void tapAndPump(final int tag, final long quitDelayMs) {
    android.os.Handler handler = new android.os.Handler(Looper.getMainLooper());
    handler.postDelayed(
        new Runnable() {
          @Override
          public void run() {
            dispatchTapForTag(tag);
          }
        },
        10);
    handler.postDelayed(
        new Runnable() {
          @Override
          public void run() {
            // Do NOT catch here: the main looper refuses to quit
            // (`quitAllowed=false`), and the thrown `IllegalStateException`
            // must propagate out of `Looper.loop()` to end this drain, exactly
            // as the W157-A mount does.
            Looper.myLooper().quit();
          }
        },
        quitDelayMs);
    try {
      Looper.loop();
      stage("pump-done", "ok");
    } catch (Throwable t) {
      stage("pump-done", esc(t.toString()));
    }
  }

  /**
   * W157-A: when true the probe runs inside an `acos-runtime` session and uses
   * the *real* `AndroidChoreographerProvider` and `AndroidInfoModule` (with the
   * `uimode`/`display` services installed by the runtime) instead of the
   * virtual provider and the service-free `PlatformConstants` shim.
   */
  public static boolean REAL_MODE = false;

  private static final String TAG = "ACOS_RN_BRIDGE ";
  private static final String OP = "ACOS_RN_OP ";

  static void emit(String json) {
    System.out.println(TAG + json);
    System.out.flush();
    try {
      android.util.Log.i("ACOS_RN_BRIDGE", json);
    } catch (Throwable ignored) {
    }
  }

  static void emitOp(String json) {
    System.out.println(OP + json);
    System.out.flush();
  }

  static void stage(String phase, String detail) {
    emit("{\"phase\":\"" + phase + "\",\"detail\":\"" + esc(detail) + "\"}");
  }

  static String esc(String s) {
    if (s == null) {
      return "";
    }
    return s.replace("\\", "\\\\").replace("\"", "'").replace("\n", " ").replace("\r", " ");
  }

  /** Print the full cause chain + frames so the first failing frame is evidence. */
  static void dump(Throwable t) {
    int depth = 0;
    while (t != null && depth < 6) {
      emit("{\"phase\":\"exception\",\"depth\":" + depth + ",\"class\":\"" + esc(t.getClass().getName()) + "\",\"message\":\"" + esc(t.getMessage()) + "\"}");
      StackTraceElement[] frames = t.getStackTrace();
      for (int i = 0; i < frames.length && i < 24; i++) {
        emit("{\"phase\":\"frame\",\"depth\":" + depth + ",\"frame\":\"" + esc(frames[i].toString()) + "\"}");
      }
      t = t.getCause();
      depth++;
    }
  }

  /** W143-A: log the exact throwable seen by the looper drain. */
  static void dumpAt(String where, Throwable t) {
    try {
      emit(
          "{\"phase\":\"drain-exception\",\"at\":\""
              + esc(where)
              + "\",\"class\":\""
              + esc(t.getClass().getName())
              + "\",\"toString\":\""
              + esc(String.valueOf(t))
              + "\"}");
    } catch (Throwable ignored) {
    }
  }

  private static void load(String name) throws Throwable {
    System.loadLibrary(name);
    stage("load", name);
  }

  interface ManagerFactory {
    ViewManager create() throws Throwable;
  }

  private static void tryAddViewManager(
      List<ViewManager> out, String name, ManagerFactory factory) {
    try {
      out.add(factory.create());
      stage("vm", name + " ok");
    } catch (Throwable t) {
      emit("{\"phase\":\"vm-skip\",\"name\":\"" + name + "\",\"error\":\"" + esc(t.toString()) + "\"}");
    }
  }

  interface ModuleFactory {
    NativeModule create() throws Throwable;
  }

  private static void putModule(
      Map<String, ModuleHolder> out, String name, ModuleFactory factory) {
    try {
      ModuleHolder holder = new ModuleHolder(factory.create());
      out.put(name, holder);
      stage("module", name + " ok");
    } catch (Throwable t) {
      emit("{\"phase\":\"module-skip\",\"name\":\"" + name + "\",\"error\":\"" + esc(t.toString()) + "\"}");
      dump(t);
    }
  }

  /** A context that stands in for the host Activity/Application. */
  public static final class ProbeContext extends ContextWrapper {
    /** Guest path (VM sandbox namespace), e.g. {@code /data/user/0/<pkg>/lib}. */
    private final String libDir;
    /** Guest path of the base APK, used only for `File`-based existence checks. */
    private final String sourceDir;
    /** Host APK path handed to the `AssetManager` native, which is host-based. */
    private final String assetApkPath;
    private final AssetManager assets;

    public ProbeContext(String libDir, String sourceDir, String assetApkPath) {
      super(null);
      this.libDir = libDir;
      this.sourceDir = sourceDir;
      this.assetApkPath = assetApkPath;
      AssetManager am = new AssetManager();
      boolean ok = false;
      try {
        ok = am.addAssetPath(assetApkPath) != 0;
        stage("assets", "addAssetPath=" + ok + " apk=" + assetApkPath);
      } catch (Throwable t) {
        emit("{\"phase\":\"assets-error\",\"error\":\"" + esc(t.toString()) + "\"}");
      }
      this.assets = ok ? am : null;
    }

    /** The APK's AssetManager, or null when {@code addAssetPath} failed. */
    public AssetManager assets() {
      return assets;
    }

    @Override
    public AssetManager getAssets() {
      return assets;
    }

    @Override
    public Context getApplicationContext() {
      return this;
    }

    /**
     * W157-A: resolve the real `UiModeManager` through the framework's own
     * `ServiceManager` + `IUiModeManager.Stub.asInterface` path, so the real
     * `AndroidInfoModule.uiMode()` reads the `uimode` service the runtime
     * installs. The hidden `IUiModeManager`/`UiModeManager(Context, service)`
     * APIs are reached reflectively because the probe is compiled against the
     * public `android.jar`.
     */
    @Override
    public Object getSystemService(String name) {
      if ("uimode".equals(name)) {
        try {
          Class<?> sm = Class.forName("android.os.ServiceManager");
          IBinder b = (IBinder) sm.getMethod("getService", String.class).invoke(null, "uimode");
          stage("uimode-service", b == null ? "null" : "binder");
          if (b == null) {
            return null;
          }
          // The real API-34 `UiModeManager(Context)` constructor resolves
          // `IUiModeManager` itself from `ServiceManager`; the runtime's
          // `uimode` service backs it. (The `(Context, IUiModeManager)`
          // constructor does not exist on this boot classpath.)
          java.lang.reflect.Constructor<android.app.UiModeManager> ctor =
              android.app.UiModeManager.class.getDeclaredConstructor(Context.class);
          ctor.setAccessible(true);
          return ctor.newInstance(this);
        } catch (Throwable t) {
          emit("{\"phase\":\"uimode-error\",\"error\":\"" + esc(t.toString()) + "\"}");
          dump(t);
          return null;
        }
      }
      return super.getSystemService(name);
    }

    @Override
    public Resources getResources() {
      return Resources.getSystem();
    }

    @Override
    public String getPackageName() {
      return "dev.acos.fixtures.rnhello";
    }

    @Override
    public ClassLoader getClassLoader() {
      return BridgeProbe.class.getClassLoader();
    }

    @Override
    public ApplicationInfo getApplicationInfo() {
      ApplicationInfo ai = new ApplicationInfo();
      ai.packageName = getPackageName();
      // `sourceDir`/`nativeLibraryDir` are guest paths: `SoLoader` canonicalizes
      // and existence-checks them through `java.io.File`, which the VM resolves
      // in its sandbox VFS. The host APK path is used only by the AssetManager
      // native (`assetApkPath`), which is host-based.
      ai.sourceDir = sourceDir;
      ai.publicSourceDir = sourceDir;
      ai.nativeLibraryDir = libDir;
      ai.metaData = new Bundle();
      // `FLAG_EXTRACT_NATIVE_LIBS` (0x10000000): the fixture libs are already
      // extracted into `nativeLibraryDir`, so SoLoader must use the
      // ApplicationSoSource (DirectorySoSource) instead of DirectApkSoSource,
      // which would re-open the APK.
      ai.flags = 0x10000000;
      return ai;
    }
  }

  /**
   * `SoLoader` file loader backed by the VM's own `System.loadLibrary` path:
   * every `.so` is already mapped, so this is a de-duplicated no-op. It exists
   * only to let `SoLoader.init` skip loading its own native `libsoloader`.
   */
  public static final class ProbeSoFileLoader
      implements com.facebook.soloader.SoFileLoader {
    @Override
    public void load(String path, int flags) {
      String name = path;
      int slash = name.lastIndexOf('/');
      if (slash >= 0) {
        name = name.substring(slash + 1);
      }
      if (name.startsWith("lib")) {
        name = name.substring(3);
      }
      if (name.endsWith(".so")) {
        name = name.substring(0, name.length() - 3);
      }
      System.loadLibrary(name);
      stage("soloader-load", name);
    }

    @Override
    public void loadBytes(
        String path, com.facebook.soloader.ElfByteChannel channel, int flags) {
      throw new UnsupportedOperationException("loadBytes");
    }
  }

  /**
   * Real `UIManagerModule` subclass: records every JS-originated op as a
   * structured JSON line, then forwards to the real implementation. Forwarding
   * is what proves the native module environment is real; the log is what the
   * test asserts.
   */
  public static final class LoggingUIManagerModule extends UIManagerModule {
    private final List<String> ops = new ArrayList<>();
    private boolean forward = false;

    LoggingUIManagerModule(
        BridgeReactContext context, List<ViewManager> viewManagers, int minTimeLeftInFrame) {
      super(context, viewManagers, minTimeLeftInFrame);
    }

    private void log(String op) {
      synchronized (ops) {
        ops.add(op);
      }
      emitOp(op);
    }

    public List<String> ops() {
      synchronized (ops) {
        return new ArrayList<>(ops);
      }
    }

    /**
     * W157-A: the tag of the view whose props carry {@code testID}, read back
     * from the recorded ops so a synthetic touch can target the real Button.
     */
    public int tagForTestId(String testId) {
      String needle = "\"testID\":\"" + testId + "\"";
      synchronized (ops) {
        for (String op : ops) {
          if (!op.contains(needle)) {
            continue;
          }
          int i = op.indexOf("\"tag\":");
          if (i < 0) {
            continue;
          }
          int j = i + 6;
          int k = j;
          while (k < op.length() && (Character.isDigit(op.charAt(k)) || op.charAt(k) == '-')) {
            k++;
          }
          try {
            return Integer.parseInt(op.substring(j, k));
          } catch (NumberFormatException ignored) {
          }
        }
      }
      return -1;
    }

    @Override
    public Map<String, Object> getConstants() {
      Map<String, Object> c = super.getConstants();
      emit("{\"phase\":\"uimanager-getConstants\",\"keys\":\"" + esc(c.keySet().toString()) + "\"}");
      return c;
    }

    @Override
    @ReactMethod
    public void createView(int tag, String className, int rootTag, ReadableMap props) {
      log("{\"op\":\"createView\",\"tag\":" + tag + ",\"class\":\"" + className + "\",\"rootTag\":" + rootTag + ",\"props\":" + json(props) + "}");
      if (forward) {
        super.createView(tag, className, rootTag, props);
      }
    }

    @Override
    @ReactMethod
    public void updateView(int tag, String className, ReadableMap props) {
      log("{\"op\":\"updateView\",\"tag\":" + tag + ",\"class\":\"" + className + "\",\"props\":" + json(props) + "}");
      if (forward) {
        super.updateView(tag, className, props);
      }
    }

    @Override
    @ReactMethod
    public void setChildren(int viewTag, ReadableArray childrenTags) {
      log("{\"op\":\"setChildren\",\"tag\":" + viewTag + ",\"children\":" + json(childrenTags) + "}");
      if (forward) {
        super.setChildren(viewTag, childrenTags);
      }
    }

    @Override
    @ReactMethod
    public void manageChildren(
        int viewTag,
        ReadableArray moveFrom,
        ReadableArray moveTo,
        ReadableArray addChildTags,
        ReadableArray addAtIndices,
        ReadableArray removeFrom) {
      log("{\"op\":\"manageChildren\",\"tag\":" + viewTag + ",\"add\":" + json(addChildTags) + "}");
      if (forward) {
        super.manageChildren(viewTag, moveFrom, moveTo, addChildTags, addAtIndices, removeFrom);
      }
    }

    @Override
    @ReactMethod
    public void removeRootView(int rootViewTag) {
      log("{\"op\":\"removeRootView\",\"tag\":" + rootViewTag + "}");
      if (forward) {
        super.removeRootView(rootViewTag);
      }
    }

    // W157-A: the JS responder system (`Pressability._measureResponderRegion`)
    // needs the target's frame before it will fire `onPress`. The probe registers
    // no view managers, so there is no native shadow tree to measure; the probe
    // answers with a synthetic 100x100 frame at the origin, which contains the
    // synthetic touch. The touch itself still travels the real `TouchEvent` ->
    // `RCTEventEmitter.receiveTouches` path.
    @Override
    @ReactMethod
    public void measure(int reactTag, com.facebook.react.bridge.Callback callback) {
      log("{\"op\":\"measure\",\"tag\":" + reactTag + "}");
      if (callback != null) {
        callback.invoke(0.0, 0.0, 100.0, 100.0, 0.0, 0.0);
      }
    }

    @Override
    @ReactMethod
    public void measureInWindow(int reactTag, com.facebook.react.bridge.Callback callback) {
      log("{\"op\":\"measureInWindow\",\"tag\":" + reactTag + "}");
      if (callback != null) {
        callback.invoke(0.0, 0.0, 100.0, 100.0);
      }
    }

    @Override
    @ReactMethod
    public void measureLayout(
        int reactTag,
        int ancestorReactTag,
        com.facebook.react.bridge.Callback success,
        com.facebook.react.bridge.Callback failure) {
      log("{\"op\":\"measureLayout\",\"tag\":" + reactTag + "}");
      if (success != null) {
        success.invoke(0.0, 0.0, 100.0, 100.0);
      }
    }

    // W157-A: `onPressIn` asks the native Button for its ripple/hotspot update.
    // The probe has no view managers, so the command is recorded and dropped;
    // it does not affect the press -> `onPress` -> `setCount` path.
    @Override
    @ReactMethod
    public void dispatchViewManagerCommand(
        int reactTag,
        com.facebook.react.bridge.Dynamic commandId,
        com.facebook.react.bridge.ReadableArray commandArgs) {
      log("{\"op\":\"dispatchViewManagerCommand\",\"tag\":" + reactTag + "}");
    }

    // W157-A: the JS responder system blocks native gesture recognisers while a
    // JS responder is active. The probe has no native gesture layer, so these
    // are recorded no-ops.
    @Override
    @ReactMethod
    public void setJSResponder(int reactTag, boolean blockNativeResponder) {
      log("{\"op\":\"setJSResponder\",\"tag\":" + reactTag + "}");
    }

    @Override
    @ReactMethod
    public void clearJSResponder() {
      log("{\"op\":\"clearJSResponder\"}");
    }
  }

  /**
   * W143-A: a `PlatformConstants` module that does not need system services.
   *
   * `AndroidInfoModule.uiMode()` calls
   * `Context.getSystemService(UI_MODE_SERVICE)`; the probe's `ProbeContext` is a
   * `ContextWrapper` with a null base, so `ContextWrapper.getSystemService`
   * throws a frame-less NPE and the whole `setUpDefaltReactNativeEnvironment`
   * bundle bootstrap aborts. Returning the constants directly keeps the real
   * `Build` values and reports a phone (`normal`) UI mode.
   */
  public static final class SimplePlatformConstantsModule extends BaseJavaModule {
    @Override
    public String getName() {
      return "PlatformConstants";
    }

    @Override
    public Map<String, Object> getConstants() {
      Map<String, Object> c = new HashMap<>();
      c.put("Version", Build.VERSION.SDK_INT);
      c.put("Release", Build.VERSION.RELEASE);
      c.put("Serial", Build.SERIAL);
      c.put("Fingerprint", Build.FINGERPRINT);
      c.put("Model", Build.MODEL);
      c.put("Manufacturer", Build.MANUFACTURER);
      c.put("Brand", Build.BRAND);
      c.put("isTesting", false);
      c.put("uiMode", "normal");
      Map<String, Object> rnv = new HashMap<>();
      rnv.put("major", 0);
      rnv.put("minor", 81);
      rnv.put("patch", 4);
      rnv.put("prerelease", null);
      c.put("reactNativeVersion", rnv);
      return c;
    }

    // W153-A: the spec's sync `getAndroidID` is registered now that JNI
    // `FromReflectedMethod` is implemented. Registering it exercises the
    // `JavaNativeModule::getMethods()` -> `MethodInvoker` path at startup;
    // returning a fixed value keeps the probe deterministic.
    @ReactMethod(isBlockingSynchronousMethod = true)
    public String getAndroidID() {
      return "acos-probe";
    }
  }

  /**
   * W143-A: a logging replacement for `ExceptionsManager`.
   *
   * The real module is a generated TurboModule spec whose `reportException`
   * path is swallowed by `ReleaseDevSupportManager`; overrides on a subclass are
   * also skipped by `JavaModuleWrapper.findMethods` (it inspects the
   * TurboModule superclass). This plain `BaseJavaModule` has the same name and
   * method descriptors, so the JS `ExceptionsManager` finds it through
   * `NativeModules`, while every JS error is emitted for the harness.
   */
  public static final class LoggingExceptionsManagerModule extends BaseJavaModule {
    @Override
    public String getName() {
      return "ExceptionsManager";
    }

    @ReactMethod
    public void reportFatalException(String message, ReadableArray stack, double id) {
      emit("{\"phase\":\"js-fatal\",\"message\":\"" + esc(message) + "\"}");
      dumpJsStack(stack);
    }

    @ReactMethod
    public void reportSoftException(String message, ReadableArray stack, double id) {
      emit("{\"phase\":\"js-soft\",\"message\":\"" + esc(message) + "\"}");
      dumpJsStack(stack);
    }

    @ReactMethod
    public void reportException(ReadableMap data) {
      emit("{\"phase\":\"js-exception\",\"data\":\"" + esc(String.valueOf(data)) + "\"}");
    }

    @ReactMethod
    public void dismissRedbox() {}

    private static void dumpJsStack(ReadableArray stack) {
      if (stack == null) {
        return;
      }
      for (int i = 0; i < stack.size() && i < 30; i++) {
        try {
          ReadableMap frame = stack.getMap(i);
          emit("{\"phase\":\"js-frame\",\"frame\":\"" + esc(String.valueOf(frame)) + "\"}");
        } catch (Throwable ignored) {
        }
      }
    }
  }

  private static String json(ReadableMap m) {
    if (m == null) {
      return "null";
    }
    try {
      WritableMap copy = Arguments.createMap();
      com.facebook.react.bridge.ReadableMapKeySetIterator it = m.keySetIterator();
      while (it.hasNextKey()) {
        String k = it.nextKey();
        switch (m.getType(k)) {
          case String:
            copy.putString(k, m.getString(k));
            break;
          case Number:
            copy.putDouble(k, m.getDouble(k));
            break;
          case Boolean:
            copy.putBoolean(k, m.getBoolean(k));
            break;
          case Null:
            copy.putNull(k);
            break;
          default:
            copy.putString(k, "<" + m.getType(k) + ">");
        }
      }
      return copy.toString();
    } catch (Throwable t) {
      return "\"<unreadable:" + esc(t.toString()) + ">\"";
    }
  }

  private static String json(ReadableArray a) {
    if (a == null) {
      return "null";
    }
    try {
      StringBuilder sb = new StringBuilder("[");
      for (int i = 0; i < a.size(); i++) {
        if (i > 0) {
          sb.append(',');
        }
        switch (a.getType(i)) {
          case Number:
            sb.append(a.getDouble(i));
            break;
          case String:
            sb.append('"').append(esc(a.getString(i))).append('"');
            break;
          case Boolean:
            sb.append(a.getBoolean(i));
            break;
          default:
            sb.append("\"<").append(a.getType(i)).append(">\"");
        }
      }
      return sb.append(']').toString();
    } catch (Throwable t) {
      return "\"<unreadable:" + esc(t.toString()) + ">\"";
    }
  }

  /**
   * Initialise the real `SoLoader` against the APK's extracted native library
   * directory. `ApplicationSoSource` wraps a `DirectorySoSource` over
   * `ApplicationInfo.nativeLibraryDir`, so `libhermes_executor.so` (merged into
   * `libhermestooling.so`) and its dependencies resolve without opening the
   * APK. Every load goes through the VM's `Runtime.nativeLoad` seam
   * (`ProbeSoFileLoader` -> `System.loadLibrary` -> `acos-jni`).
   *
   * The external mapping (`OpenSourceMergedSoMapping`) is installed first:
   * `SoLoader.initEnableConfig` returns early when it is non-null, and the
   * mapping translates merged names (`hermes_executor` -> `hermestooling`) and
   * drives the merged `JNI_OnLoad` calls. That also avoids `PackageManager`
   * discovery, which the probe context does not provide.
   */
  private static void initSoLoader(ProbeContext context) throws Throwable {
    java.lang.reflect.Field ext = SoLoader.class.getDeclaredField("externalSoMapping");
    ext.setAccessible(true);
    ext.set(null, OpenSourceMergedSoMapping.INSTANCE);
    stage("soloader-ext", String.valueOf(ext.get(null)));
    stage("soloader", "before-init");
    try {
      SoLoader.init(context, 0, new ProbeSoFileLoader());
    } catch (Throwable t) {
      stage("soloader-init-error", t.toString());
      dump(t);
      throw t;
    }
    stage("soloader", "init ok");
    stage("soloader-isinit", String.valueOf(SoLoader.isInitialized()));
    for (SoSource source : SoLoader.cloneSoSources()) {
      stage("soloader-source", source.getName());
    }
  }

  /**
   * W143-A: a self-driven `ChoreographerProvider`.
   *
   * `AndroidChoreographerProvider.getChoreographer()` builds a real
   * `android.view.Choreographer`, whose constructor calls
   * `DisplayManagerGlobal.getInstance()` ->
   * `ServiceManager.rawGetService("display")`. The probe VM has no binder
   * display service, so `rawGetService` throws a frame-less NPE (diagnosed in
   * `results/rn/W143A_BRIDGE.md`). That is display/ServiceManager native work,
   * outside this harness.
   *
   * Frame callbacks are what drive RN's `TimingModule` and the legacy
   * `UIManagerModule`'s `DISPATCH_UI` flush, so simply dropping them stalls the
   * React commit and no `UIManager` op ever reaches the bridge. This provider
   * posts each requested frame back onto the main looper with a synthetic
   * 16 ms clock, so the VM's virtual-time scheduler drives frames exactly like
   * it drives the looper.
   */
  public static final class VirtualChoreographerProvider implements ChoreographerProvider {
    public static final VirtualChoreographerProvider INSTANCE =
        new VirtualChoreographerProvider();

    private static final class Virtual implements ChoreographerProvider.Choreographer {
      private long frameNanos = 0;

      @Override
      public void postFrameCallback(android.view.Choreographer.FrameCallback callback) {
        new android.os.Handler(Looper.getMainLooper())
            .post(
                () -> {
                  frameNanos += 16_000_000L;
                  callback.doFrame(frameNanos);
                });
      }

      @Override
      public void removeFrameCallback(android.view.Choreographer.FrameCallback callback) {}
    }

    @Override
    public ChoreographerProvider.Choreographer getChoreographer() {
      return new Virtual();
    }
  }

  /**
   * Prepare the process main looper and the `ReactChoreographer` singleton.
   *
   * Real Android does this in `ActivityThread.main` (`Looper.prepareMainLooper`)
   * and in `ReactInstanceManager` (`ReactChoreographer.initialize`); the probe
   * runs `main` on the VM's thread 0, so it must do both itself. Without a main
   * `Looper`, `MessageQueueThreadImpl.createForMainThread` fails with
   * `getMainLooper(...) must not be null` (W123-A). The main looper is a real
   * `MessageQueue` (native `nativeInit`), so the VM's looper pump can drive it
   * with virtual time, exactly like the framework lifecycle driver (W118-A).
   */
  private static void prepareMainThread(boolean real) {
    try {
      if (Looper.myLooper() == null) {
        Looper.prepareMainLooper();
      }
      stage("main-looper", String.valueOf(Looper.getMainLooper()));
    } catch (Throwable t) {
      emit("{\"phase\":\"main-looper-error\",\"error\":\"" + esc(t.toString()) + "\"}");
      dump(t);
    }
    try {
      if (real) {
        // W157-A: the real provider builds `android.view.Choreographer`, whose
        // constructor reads the `display` service and whose vsync source is the
        // runtime's `DisplayEventReceiver`; no synthetic clock is involved.
        ReactChoreographer.initialize(AndroidChoreographerProvider.INSTANCE);
        stage("choreographer", "ok-real-provider");
      } else {
        ReactChoreographer.initialize(VirtualChoreographerProvider.INSTANCE);
        stage("choreographer", "ok-virtual-provider");
      }
    } catch (Throwable t) {
      emit("{\"phase\":\"choreographer-error\",\"error\":\"" + esc(t.toString()) + "\"}");
      dump(t);
    }
  }

  /**
   * W157-A: dispatch a synthetic tap through RN's real event path.
   *
   * `JSTouchDispatcher` resolves a target from the native `ViewGroup`
   * hierarchy; the probe registers no view managers, so the probe emits the
   * same `TouchEvent`s `JSTouchDispatcher` would (`START` then `END`) directly
   * through the real `UIManagerModule.getEventDispatcher()`. The JS responder
   * system then routes the press to the Button's `onPress`, and the resulting
   * re-render is captured as the second `UIManager` batch.
   */
  private static void dispatchTap(LoggingUIManagerModule um) {
    int tag = um.tagForTestId("hello-button");
    System.out.println("ACOS_RN_TAP_START {\"tag\":" + tag + "}");
    System.out.flush();
    if (tag <= 0) {
      stage("tap", "no-target-tag");
      return;
    }
    try {
      EventDispatcher ed = um.getEventDispatcher();
      TouchEventCoalescingKeyHelper helper = new TouchEventCoalescingKeyHelper();
      long t = android.os.SystemClock.uptimeMillis();
      float x = 8f;
      float y = 8f;
      MotionEvent down = MotionEvent.obtain(t, t, MotionEvent.ACTION_DOWN, x, y, 0);
      ed.dispatchEvent(TouchEvent.obtain(tag, TouchEventType.START, down, t, x, y, helper));
      MotionEvent up = MotionEvent.obtain(t, t + 40, MotionEvent.ACTION_UP, x, y, 0);
      ed.dispatchEvent(TouchEvent.obtain(tag, TouchEventType.END, up, t, x, y, helper));
      stage("tap", "dispatched tag=" + tag);
    } catch (Throwable t) {
      emit("{\"phase\":\"tap-error\",\"error\":\"" + esc(t.toString()) + "\"}");
      dump(t);
    }
  }

  public static void main(String[] args) {
    emit("{\"phase\":\"begin\"}");
    // W157-A: `real` selects the real `AndroidChoreographerProvider` +
    // `AndroidInfoModule` (runtime session); `tap` asks for the synthetic touch.
    boolean realMode = args.length > 4 && "real".equals(args[4]);
    boolean tap = args.length > 5 && "tap".equals(args[5]);
    REAL_MODE = realMode;
    prepareMainThread(realMode);
    // Earliest possible main-looper marker: proves the looper can dispatch and
    // runs before the native `initializeRuntime` task posted by `initializeBridge`.
    new android.os.Handler(Looper.getMainLooper())
        .post(() -> stage("diag", "pre-bridge"));
    String bundlePath = args.length > 0 ? args[0] : "";
    String libDir = args.length > 1 ? args[1] : "";
    String sourceDir = args.length > 2 ? args[2] : bundlePath;
    String assetApkPath = args.length > 3 ? args[3] : sourceDir;
    ProbeContext appContext = new ProbeContext(libDir, sourceDir, assetApkPath);
    try {
      initSoLoader(appContext);
    } catch (Throwable t) {
      emit("{\"phase\":\"load-error\",\"error\":\"" + esc(t.toString()) + "\"}");
      dump(t);
      return;
    }

    BridgeReactContext reactContext = null;
    LoggingUIManagerModule uiManager = null;
    CatalystInstanceImpl catalyst = null;
    try {
      java.lang.reflect.Field pf = BridgeProbe.class.getDeclaredField("PROBE_STATIC");
      pf.setAccessible(true);
      pf.set(null, "set-by-reflection");
      stage("reflect-static-self", String.valueOf(PROBE_STATIC));
      java.lang.reflect.Field paf = BridgeProbe.class.getDeclaredField("PROBE_ARR");
      paf.setAccessible(true);
      paf.set(null, new Object[] {"x"});
      stage("reflect-static-arr", String.valueOf(PROBE_ARR != null && PROBE_ARR.length == 1));
      java.lang.reflect.Field pvf = BridgeProbe.class.getDeclaredField("PROBE_VOL");
      pvf.setAccessible(true);
      pvf.set(null, "vol-set");
      stage("reflect-static-vol", String.valueOf(PROBE_VOL));
      java.lang.reflect.Field tagf = SoLoader.class.getDeclaredField("TAG");
      tagf.setAccessible(true);
      stage("reflect-tag", String.valueOf(tagf.get(null)) + "/" + SoLoader.TAG);
      // Deterministic display metrics: avoids a full Resources/AssetManager
      // dependency for the UIManagerModule constructor while still giving it
      // real, non-zero metrics.
      DisplayMetrics dm = new DisplayMetrics();
      dm.widthPixels = 1080;
      dm.heightPixels = 1920;
      dm.density = 3.0f;
      dm.scaledDensity = 3.0f;
      dm.densityDpi = 480;
      dm.xdpi = 480;
      dm.ydpi = 480;
      DisplayMetricsHolder.setWindowDisplayMetrics(dm);
      DisplayMetricsHolder.setScreenDisplayMetrics(dm);
      stage("metrics", "ok");

      reactContext = new BridgeReactContext(appContext);
      stage("context", "ok");

      // Only the view managers the hello tree needs. Calling
      // `MainReactPackage.createViewManagers` eagerly constructs every manager,
      // including the Fresco image managers whose static init wants
      // `SoLoader.init()` (W88-A blocker). The real classes are still the APK's.
      List<ViewManager> viewManagers = new ArrayList<>();
      // Registered view managers are optional for capturing the JS-originated
      // commands: `LoggingUIManagerModule` records every call before the real
      // `UIImplementation` resolves the view manager. W88-A records why the real
      // ones cannot be constructed here (SoLoader feature-flag accessor +
      // under-resolved fixture deps, see results/rn/W88A_BRIDGE.md).
      stage("viewmanagers", "count=" + viewManagers.size());

      uiManager = new LoggingUIManagerModule(reactContext, viewManagers, 0);
      sUiManager = uiManager;
      stage("uimanager", "ok");

      final BridgeReactContext rc = reactContext;
      final LoggingUIManagerModule um = uiManager;
      Map<String, ModuleHolder> modules = new HashMap<>();
      putModule(modules, "UIManager", () -> um);
      if (realMode) {
        // W157-A: the real TurboModule reads `uiMode` through the real
        // `UiModeManager` backed by the runtime's `uimode` service.
        putModule(modules, "PlatformConstants", () -> new AndroidInfoModule(rc));
      } else {
        putModule(modules, "PlatformConstants", () -> new SimplePlatformConstantsModule());
      }
      putModule(modules, "DeviceInfo", () -> new DeviceInfoModule(rc));
      // The real `ReleaseDevSupportManager` is RN's own no-op dev-support
      // implementation; the Kotlin constructors reject `null` (W88-A blocker 4).
      putModule(modules, "ExceptionsManager", () -> new LoggingExceptionsManagerModule());
      putModule(modules, "Timing", () -> new TimingModule(rc, new ReleaseDevSupportManager()));
      putModule(modules, "AppState", () -> new AppStateModule(rc));
      stage("modules", "count=" + modules.size());

      NativeModuleRegistry registry = new NativeModuleRegistry(reactContext, modules);
      JavaScriptExecutor jsExecutor = new HermesExecutorFactory().create();
      stage("executor", "ok");

      // The host `bundlePath` is the extracted `assets/index.android.bundle`.
      // The file loader's native `JSBigFileString::fromPath` runs in the host
      // process and opens that host path directly, whereas the asset loader
      // needs the VM's NDK `AAssetManager_open`, which cannot stream the bundle
      // out of this APK (W127-A). Both load the same Hermes bytecode.
      JSBundleLoader loader;
      if (!bundlePath.isEmpty()) {
        // `loadSynchronously` MUST be false. Native `Instance::initializeBridge`
        // calls `NativeToJsBridge::initializeRuntime()` by *posting* it to the
        // JS queue; it is that task which installs `global.nativeModuleProxy`
        // (JSIExecutor::initializeRuntime). The sync loader
        // (`NativeToJsBridge::loadBundleSync`) evaluates the bundle directly on
        // the calling thread, running it *before* the queued
        // `initializeRuntime`, so the JS `setUpDefaltReactNativeEnvironment`
        // sees no `nativeModuleProxy` and no `__fbBatchedBridgeConfig` and
        // aborts with "__fbBatchedBridgeConfig is not set". The async loader
        // posts `loadBundle` behind `initializeRuntime` on the same main-looper
        // queue; `Looper.loop()` below then drains both in order (W143-A).
        loader = JSBundleLoader.createFileLoader(bundlePath, bundlePath, /* loadSynchronously */ false);
        stage("loader", "file-async:" + bundlePath);
      } else {
        loader = JSBundleLoader.createAssetLoader(appContext, "index.android.bundle", true);
        stage("loader", "asset:index.android.bundle");
      }

      // Single-threaded bridge: run the UI, JS and native-modules roles all on
      // the main looper. Real Android gives JS/native modules their own OS
      // threads; ACOS's queues are green threads, and the native
      // `JMessageQueueThread::runOnQueueSync` (called by
      // `Instance::initializeBridge`) blocks the host in a libc++
      // `condition_variable` waiting for the JS green thread, which cannot run
      // because the host is blocked. Putting all three roles on the main looper
      // makes `isOnThread()` true so the setup runs inline; `Looper.loop()`
      // below then drains the asynchronous work.
      ReactQueueConfigurationSpec spec =
          new ReactQueueConfigurationSpec(
              MessageQueueThreadSpec.mainThreadSpec(),
              MessageQueueThreadSpec.mainThreadSpec());
      stage("spec", "single-threaded-main");
      catalyst =
          new CatalystInstanceImpl.Builder()
              .setReactQueueConfigurationSpec(spec)
              .setJSExecutor(jsExecutor)
              .setRegistry(registry)
              .setJSBundleLoader(loader)
              .setJSExceptionHandler(new DefaultJSExceptionHandler())
              .build();
      stage("catalyst", "ok");

      reactContext.initializeWithInstance(catalyst);
      stage("initializeWithInstance", "ok");
      // W143-A bisect markers: `initializeRuntime` was posted during
      // `initializeBridge`; these run on the same main-looper queue right
      // behind it and ahead of the bundle/`runApplication` tasks.
      new android.os.Handler(Looper.getMainLooper())
          .post(() -> stage("diag", "after-init"));
    } catch (Throwable t) {
      emit("{\"phase\":\"bridge-error\",\"at\":\"build\",\"error\":\"" + esc(t.toString()) + "\"}");
      dump(t);
      return;
    }

    try {
      catalyst.runJSBundle();
      stage("runJSBundle", "ok");
      new android.os.Handler(Looper.getMainLooper())
          .post(() -> stage("diag", "after-runjsbundle"));
    } catch (Throwable t) {
      emit("{\"phase\":\"bridge-error\",\"at\":\"runJSBundle\",\"error\":\"" + esc(t.toString()) + "\"}");
      dump(t);
      return;
    }

    // Mount the registered component. ReactRootView does exactly this:
    //   getJSModule(AppRegistry.class).runApplication(name, {rootTag, initialProps})
    try {
      WritableMap params = Arguments.createMap();
      params.putInt("rootTag", 0xACE5);
      params.putMap("initialProps", Arguments.createMap());
      com.facebook.react.bridge.WritableArray appParams = Arguments.createArray();
      appParams.pushString("rnhello");
      appParams.pushMap(params);
      catalyst.callFunction("AppRegistry", "runApplication", (com.facebook.react.bridge.NativeArray) appParams);
      stage("runApplication", "ok");
      new android.os.Handler(Looper.getMainLooper())
          .post(() -> stage("diag", "after-runapplication"));
    } catch (Throwable t) {
      emit("{\"phase\":\"bridge-error\",\"at\":\"runApplication\",\"error\":\"" + esc(t.toString()) + "\"}");
      dump(t);
    }

    // All three RN queues share the main looper (see the spec above). The
    // bridge's asynchronous work is posted to that looper's Handler, so drain
    // it by running `Looper.loop()` and quitting after a bounded delay. The
    // VM's virtual-time scheduler advances the guest clock to the quit message
    // so the drain is deterministic and does not wait in real time.
    try {
      android.os.Handler handler = new android.os.Handler(Looper.getMainLooper());
      handler.post(() -> stage("diag", "drain-start"));
      if (tap && uiManager != null) {
        final LoggingUIManagerModule umTap = uiManager;
        handler.postDelayed(
            new Runnable() {
              @Override
              public void run() {
                dispatchTap(umTap);
              }
            },
            400);
      }
      handler.postDelayed(
          new Runnable() {
            @Override
            public void run() {
              stage("drain", "quit");
              Looper.myLooper().quit();
            }
          },
          1500);
      Looper.loop();
      stage("looper-done", "ok");
    } catch (Throwable t) {
      // `Looper.quit()` from the main thread throws this by design; it is the
      // intended way to break our own embedded `Looper.loop()` here, not a
      // failure. Treat it as a clean drain.
      if (t instanceof IllegalStateException
          && String.valueOf(t.getMessage()).contains("Main thread not allowed to quit")) {
        stage("looper-done", "quit-sentinel");
      } else {
        stage("drain-error", esc(t.toString()));
        dumpAt("looper-loop", t);
        dump(t);
      }
    }

    if (uiManager != null) {
      stage("ops", "count=" + uiManager.ops().size());
    }
    emit("{\"phase\":\"end\"}");
  }
}
