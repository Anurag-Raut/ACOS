package dev.acos.fixtures.insetsprobe;

import android.app.Activity;
import android.graphics.Insets;
import android.graphics.Rect;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowManager;
import android.view.WindowMetrics;
import android.widget.TextView;

/**
 * W7 regression fixture: an edge-to-edge Activity that prints the window frame
 * and the insets the framework reports to it.
 *
 * <p>This is the Gaten footer defect reduced to framework-only code. Under the
 * pre-W7 ACOS model the WMS carves the app window to the display application
 * area, so {@code InsetsState.calculateInsets(mWinFrame, ...)} returns zero:
 * the frame is {@code [0,128][1080,2337]} (screen space) and the status/nav
 * rows read 0. Under {@code ACOS_WM_INSETS=report} the app owns the full
 * display {@code [0,0][1080,2400]} and the same source frames report status
 * 128 px / navigation 63 px at the default 420 dpi profile.
 *
 * <p>The Activity is edge-to-edge ({@link Window#setDecorFitsSystemWindows}
 * false, targetSdk 35) so the decor does not consume the insets; it only reads
 * and reports them. A non-edge-to-edge fixture keeps its content below the
 * status bar because {@code DecorView} consumes the same reported insets.
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosInsetsProbe";

    private TextView status;
    private TextView nav;
    private TextView systemBars;
    private TextView gestures;
    private TextView decor;
    private TextView frame;
    private TextView window;
    private TextView display;
    private TextView mode;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        // Explicit edge-to-edge on the API-34 image (targetSdk 35 enforces it
        // on Android 15, but ACOS runs the 14 image).
        getWindow().setDecorFitsSystemWindows(false);
        setContentView(R.layout.main);
        status = (TextView) findViewById(R.id.status);
        nav = (TextView) findViewById(R.id.nav);
        systemBars = (TextView) findViewById(R.id.system_bars);
        gestures = (TextView) findViewById(R.id.gestures);
        decor = (TextView) findViewById(R.id.decor);
        frame = (TextView) findViewById(R.id.frame);
        window = (TextView) findViewById(R.id.window);
        display = (TextView) findViewById(R.id.display);
        mode = (TextView) findViewById(R.id.probe_mode);

        final View decorView = getWindow().getDecorView();
        decorView.setOnApplyWindowInsetsListener(new View.OnApplyWindowInsetsListener() {
            @Override
            public WindowInsets onApplyWindowInsets(View v, WindowInsets insets) {
                render(insets);
                // Edge-to-edge: do not consume; the rest of the hierarchy sees
                // the same insets the app just reported.
                return v.onApplyWindowInsets(insets);
            }
        });
        decorView.addOnLayoutChangeListener(new View.OnLayoutChangeListener() {
            @Override
            public void onLayoutChange(View v, int left, int top, int right, int bottom,
                                       int oldLeft, int oldTop, int oldRight, int oldBottom) {
                render(decorView.getRootWindowInsets());
            }
        });
        decorView.post(new Runnable() {
            @Override
            public void run() {
                render(decorView.getRootWindowInsets());
            }
        });
    }

    private void render(WindowInsets insets) {
        if (insets == null) {
            mode.setText("mode=waiting-for-insets");
            return;
        }
        Insets st = insets.getInsets(WindowInsets.Type.statusBars());
        Insets nb = insets.getInsets(WindowInsets.Type.navigationBars());
        Insets sb = insets.getInsets(WindowInsets.Type.systemBars());
        Insets sg = insets.getInsets(WindowInsets.Type.systemGestures());
        status.setText("status=" + st.top);
        nav.setText("nav=" + nb.bottom);
        systemBars.setText("systemBars=" + sb.top + "," + sb.bottom);
        gestures.setText("gestures=" + sg.bottom);

        View decorView = getWindow().getDecorView();
        decor.setText("decor=[" + decorView.getLeft() + "," + decorView.getTop() + "]["
                + decorView.getRight() + "," + decorView.getBottom() + "]");
        int[] loc = new int[2];
        decorView.getLocationOnScreen(loc);
        frame.setText("frame=[" + loc[0] + "," + loc[1] + "]["
                + (loc[0] + decorView.getWidth()) + "," + (loc[1] + decorView.getHeight()) + "]");
        WindowManager wm = getWindowManager();
        WindowMetrics metrics = wm.getCurrentWindowMetrics();
        Rect bounds = metrics.getBounds();
        window.setText("window=[" + bounds.left + "," + bounds.top + "]["
                + bounds.right + "," + bounds.bottom + "]");
        display.setText("display=" + getResources().getDisplayMetrics().widthPixels + "x"
                + getResources().getDisplayMetrics().heightPixels);
        mode.setText("mode=edge-to-edge");
        Log.i(TAG, status.getText() + " " + nav.getText() + " " + systemBars.getText()
                + " " + gestures.getText() + " " + decor.getText() + " " + frame.getText()
                + " " + window.getText());
    }
}
