package dev.acos.fixtures.flutterhello;

import android.content.res.AssetManager;
import io.flutter.embedding.engine.FlutterJNI;
import java.util.ArrayList;
import java.util.List;

/**
 * ACOS W93-A: Java harness that drives the real Flutter embedding entry points
 * (`FlutterJNI`) exactly the way `FlutterShellArgs`/`FlutterEngine` do, so the
 * engine's registered natives run from real Java inside the ACOS VM.
 *
 * <p>It uses only the public `FlutterJNI` surface (no reflection). The fixture
 * is AOT, so `bundlePath` is the `libapp.so` path the loader extracted from the
 * APK.
 */
public final class FlutterProbe {
  private FlutterProbe() {}

  public static void main(String[] args) {
    String bundlePath = args.length > 0 ? args[0] : null;
    String entrypoint = args.length > 1 ? args[1] : "main";
    System.out.println("ACOS_FLUTTER_PROBE phase=start bundle=" + bundlePath);
    try {
      System.out.println("ACOS_FLUTTER_PROBE phase=loadLibrary begin");
      System.loadLibrary("flutter");
      System.out.println("ACOS_FLUTTER_PROBE phase=loadLibrary ok");

      FlutterJNI jni = new FlutterJNI();
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
            "--aot-shared-library-name=" + bundlePath
          };
      jni.init(null, shellArgs, null, null, null, System.currentTimeMillis(), 0);
      System.out.println("ACOS_FLUTTER_PROBE phase=init ok");

      jni.attachToNative();
      System.out.println("ACOS_FLUTTER_PROBE phase=attach ok");

      List<String> dartArgs = new ArrayList<String>();
      jni.runBundleAndSnapshotFromLibrary(
          bundlePath, entrypoint, null, (AssetManager) null, dartArgs, 0L);
      System.out.println("ACOS_FLUTTER_PROBE phase=runBundle ok");
    } catch (Throwable t) {
      System.out.println("ACOS_FLUTTER_PROBE phase=error " + t);
      t.printStackTrace(System.out);
    }
  }
}
