package dev.acos.fixtures.rnhello;

import com.facebook.react.ReactActivity;
import com.facebook.react.ReactActivityDelegate;
import com.facebook.react.defaults.DefaultReactActivityDelegate;

/**
 * Single launcher activity. The JS component registered under "rnhello" is
 * mounted by the legacy {@code ReactRootView} + {@code UIManagerModule}
 * pipeline (Fabric explicitly disabled) — that is the path ACOS must emulate
 * to render the first {@code Text} as a real {@code android.widget.TextView}.
 */
public class MainActivity extends ReactActivity {

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
