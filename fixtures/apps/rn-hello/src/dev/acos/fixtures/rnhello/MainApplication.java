package dev.acos.fixtures.rnhello;

import android.app.Application;

import com.facebook.react.ReactApplication;
import com.facebook.react.ReactNativeHost;
import com.facebook.react.ReactPackage;
import com.facebook.react.defaults.DefaultReactNativeHost;
import com.facebook.react.shell.MainReactPackage;
import com.facebook.react.soloader.OpenSourceMergedSoMapping;
import com.facebook.soloader.ElfByteChannel;
import com.facebook.soloader.SoFileLoader;
import com.facebook.soloader.SoLoader;

import java.io.IOException;
import java.lang.reflect.Field;
import java.util.Arrays;
import java.util.List;

/**
 * Host application. Mirrors the RN 0.81 template but does not depend on the
 * codegen-generated {@code PackageList}/{@code BuildConfig}: it uses
 * {@link MainReactPackage} directly and hard-codes the two switches:
 *
 * <ul>
 *   <li>{@code isNewArchEnabled() == false} — legacy Paper architecture,</li>
 *   <li>{@code isHermesEnabled() == true} — load {@code libhermes.so}.</li>
 * </ul>
 */
public class MainApplication extends Application implements ReactApplication {

  private final ReactNativeHost mReactNativeHost =
      new DefaultReactNativeHost(this) {
        @Override
        public boolean getUseDeveloperSupport() {
          return false;
        }

        @Override
        protected List<ReactPackage> getPackages() {
          // No autolinked packages in this fixture.
          return Arrays.<ReactPackage>asList(new MainReactPackage());
        }

        @Override
        protected String getJSMainModuleName() {
          return "index";
        }

        @Override
        protected boolean isNewArchEnabled() {
          return false;
        }

        @Override
        protected boolean isHermesEnabled() {
          return true;
        }
      };

  @Override
  public ReactNativeHost getReactNativeHost() {
    return mReactNativeHost;
  }

  @Override
  public void onCreate() {
    super.onCreate();
    // RN 0.81 merges its separate JNI libraries (react_featureflagsjni,
    // reactnativejni, fabricjni, ...) into libreactnative.so. Register the OSS
    // merged-so mapping so `SoLoader.loadLibrary` redirects those logical names
    // (otherwise the first `loadLibrary("react_featureflagsjni")` looks for a
    // `libreact_featureflagsjni.so` that does not exist).
    //
    // `SoLoader` has no public API to set the mapping *and* a `SoFileLoader` at
    // once, so the private mapping field is installed by reflection first (the
    // same seam the `BridgeProbe` native-session harness uses), then `init` is
    // called with the ACOS file loader below. The host `SoLoader` default calls
    // `System.load(<absolute path>)`; ACOS's confined native loader resolves a
    // library by name against the app's extracted lib directory, so the loader
    // strips the path to a bare name and calls `System.loadLibrary`.
    try {
      Field ext = SoLoader.class.getDeclaredField("externalSoMapping");
      ext.setAccessible(true);
      ext.set(null, OpenSourceMergedSoMapping.INSTANCE);
      SoLoader.init(this, 0, new FixtureSoFileLoader());
    } catch (IOException | ReflectiveOperationException e) {
      throw new RuntimeException("SoLoader init failed", e);
    }
  }

  /**
   * `SoLoader` file loader backed by the VM's `System.loadLibrary` path.
   *
   * `DirectorySoSource` hands the loader the canonical *host* path of an
   * extracted `.so`; the ACOS native loader refuses an absolute/separator path
   * from guest code and only resolves a plain library name against the JNI
   * search path (the app's extracted `nativeLibraryDir`). Strip the path back
   * to `foo`/`libfoo.so` and let the VM resolve it.
   */
  static final class FixtureSoFileLoader implements SoFileLoader {
    @Override
    public void load(String path, int flags) {
      String name = path.substring(path.lastIndexOf('/') + 1);
      if (name.startsWith("lib")) {
        name = name.substring(3);
      }
      if (name.endsWith(".so")) {
        name = name.substring(0, name.length() - 3);
      }
      System.loadLibrary(name);
    }

    @Override
    public void loadBytes(String path, ElfByteChannel channel, int flags) {
      throw new UnsupportedOperationException("loadBytes");
    }
  }
}
