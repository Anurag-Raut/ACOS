package dev.acos.fixtures.overlaystack;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Context;
import android.graphics.PixelFormat;
import android.os.Bundle;
import android.util.Log;
import android.view.Gravity;
import android.view.View;
import android.view.WindowManager;
import android.widget.TextView;

/**
 * LAYER1 window-stack fixture: one Activity, one {@link AlertDialog} and two
 * {@code SYSTEM_ALERT_WINDOW} application overlays.
 *
 * <p>Observable behaviour:
 * <ul>
 *   <li>on create it adds overlay A (touchable, flat red at display
 *       {@code [40,648][520,908]}) and overlay B
 *       ({@code FLAG_NOT_TOUCHABLE}, flat blue at {@code [560,1328][1040,1628]});
 *   <li>the "Show dialog" button raises an {@link AlertDialog} between the
 *       activity and the overlays;
 *   <li>a tap on overlay A logs {@code overlay_tapped}; a tap that the
 *       activity receives (beside the overlay, or through the not-touchable
 *       one) logs {@code activity_tapped x=.. y=..}.
 * </ul>
 *
 * <p>The session grants {@code SYSTEM_ALERT_WINDOW} because the manifest
 * declares it, which is what makes {@code TYPE_APPLICATION_OVERLAY} legal.
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosOverlay";

    /** `WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY`. */
    private static final int TYPE_APPLICATION_OVERLAY = 2038;
    /** `FLAG_NOT_FOCUSABLE`: an overlay must not steal the activity's focus. */
    private static final int FLAG_NOT_FOCUSABLE = 0x8;
    /** `FLAG_NOT_TOUCHABLE`: touches pass through to the window below. */
    private static final int FLAG_NOT_TOUCHABLE = 0x10;
    /** `FLAG_NOT_TOUCH_MODAL`: touches outside the frame go to the window below. */
    private static final int FLAG_NOT_TOUCH_MODAL = 0x20;

    private WindowManager windowManager;
    private TextView status;
    private View overlayA;
    private View overlayB;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        windowManager = (WindowManager) getSystemService(Context.WINDOW_SERVICE);

        status = (TextView) findViewById(R.id.status);
        findViewById(R.id.root).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                // A visible change, so the strict settle rule sees a draw: the
                // log alone is not a frame.
                status.setText("activity tapped");
                Log.i(TAG, "activity_tapped");
            }
        });
        findViewById(R.id.show_dialog).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showDialog();
            }
        });

        overlayA = addOverlay(0xFFE02020, 40, 520, 480, 260, true);
        overlayB = addOverlay(0xFF2040E0, 560, 1200, 480, 300, false);
        Log.i(TAG, "overlays_added a_touchable=true b_touchable=false");
    }

    /**
     * Add one application overlay. {@code touchable == false} sets
     * {@code FLAG_NOT_TOUCHABLE}; every overlay is {@code FLAG_NOT_FOCUSABLE}
     * and {@code FLAG_NOT_TOUCH_MODAL} (the stock shape of a
     * {@code SYSTEM_ALERT_WINDOW} decoration).
     */
    private View addOverlay(int color, int x, int y, int w, int h, boolean touchable) {
        View view = new View(this);
        view.setBackgroundColor(color);
        WindowManager.LayoutParams params = new WindowManager.LayoutParams(
                w, h, TYPE_APPLICATION_OVERLAY,
                FLAG_NOT_FOCUSABLE | FLAG_NOT_TOUCH_MODAL
                        | (touchable ? 0 : FLAG_NOT_TOUCHABLE),
                PixelFormat.TRANSLUCENT);
        params.gravity = Gravity.TOP | Gravity.LEFT;
        params.x = x;
        params.y = y;
        if (touchable) {
            view.setClickable(true);
            view.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    // The overlay's own surface changes colour: a real draw the
                    // settle rule can measure.
                    v.setBackgroundColor(0xFFE06060);
                    Log.i(TAG, "overlay_tapped");
                }
            });
        }
        windowManager.addView(view, params);
        return view;
    }

    private void showDialog() {
        new AlertDialog.Builder(this)
                .setTitle("Overlay stack")
                .setMessage("Activity < dialog < application overlay")
                .setPositiveButton("OK", null)
                .show();
        Log.i(TAG, "dialog_shown");
    }

    @Override
    protected void onDestroy() {
        // An overlay outlives the activity unless it is removed explicitly.
        for (View overlay : new View[]{overlayA, overlayB}) {
            if (overlay == null) continue;
            try {
                windowManager.removeView(overlay);
            } catch (Throwable ignored) {
            }
        }
        overlayA = null;
        overlayB = null;
        super.onDestroy();
    }
}
