package dev.acos.fixtures.rnhello;

import android.os.Bundle;

import com.facebook.react.ReactActivity;
import com.facebook.react.ReactActivityDelegate;
import com.facebook.react.defaults.DefaultNewArchitectureEntryPoint;
import com.facebook.react.defaults.DefaultReactActivityDelegate;

/**
 * Single launcher activity. The JS component registered under "rnhello" is
 * mounted by the legacy {@code ReactRootView} + {@code UIManagerModule}
 * pipeline (Fabric explicitly disabled) — that is the path ACOS must emulate
 * to render the first {@code Text} as a real {@code android.widget.TextView}.
 */
public class MainActivity extends ReactActivity {

  @Override
  protected void onCreate(Bundle savedInstanceState) {
    // Apply this app's architecture choice to React Native's C++ feature flags
    // before `ReactActivity.onCreate` -> `ReactDelegate.loadApp` reads them.
    // RN 0.81's native default for `enableBridgelessArchitecture()` is true; a
    // real Paper app clears it through `DefaultNewArchitectureEntryPoint.load`
    // (the template calls it when `IS_NEW_ARCHITECTURE_ENABLED` is false).
    // Without this, `ReactDelegate.loadApp` takes the bridgeless branch,
    // `reactHost` is null (this fixture is Paper: `isNewArchEnabled()` is
    // false and there is no `ReactHost`), `internalReactRootView` is never
    // created, and `Activity.setContentView(null)` throws.
    //
    // This runs on the Activity thread, not `Application.onCreate`: the flag
    // override is a JNI call (`ReactNativeFeatureFlagsCxxInterop.override`)
    // that requires an attached JNI environment, and the W178 merged
    // `libreactnative.so` load proves the Activity thread is attached while
    // the Application bind thread is not. `load(turboModules, fabric,
    // bridgeless)` all-false is the Paper configuration.
    DefaultNewArchitectureEntryPoint.load(false, false, false);
    super.onCreate(savedInstanceState);
  }

  @Override
  protected String getMainComponentName() {
    return "rnhello";
  }

  @Override
  protected ReactActivityDelegate createReactActivityDelegate() {
    // false => Fabric disabled => legacy Paper UIManager (TextView etc.).
    return new DefaultReactActivityDelegate(this, getMainComponentName(), false);
  }
}
