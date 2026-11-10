package dev.acos.fixtures.rnhello;

import android.app.Application;

import com.facebook.react.ReactApplication;
import com.facebook.react.ReactNativeHost;
import com.facebook.react.ReactPackage;
import com.facebook.react.defaults.DefaultReactNativeHost;
import com.facebook.react.shell.MainReactPackage;
import com.facebook.soloader.SoLoader;

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
    SoLoader.init(this, /* native exopackage */ false);
  }
}
