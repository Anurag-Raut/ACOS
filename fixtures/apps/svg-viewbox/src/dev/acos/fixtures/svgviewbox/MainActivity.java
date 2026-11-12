package dev.acos.fixtures.svgviewbox;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;

/**
 * Host for {@link SvgProbeView}: a plain framework Activity with no native code, no AndroidX and
 * no SVG library. The probe reproduces the arithmetic react-native-svg's Android renderer performs
 * (SvgView.getViewBox / ViewBox.getTransform / PathParser's density-scaled path) so an ACOS guest
 * can be compared against a physical device for the same numbers.
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosSvgProbe";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
    }
}
