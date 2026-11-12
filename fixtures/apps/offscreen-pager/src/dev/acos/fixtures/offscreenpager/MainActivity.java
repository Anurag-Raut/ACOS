package dev.acos.fixtures.offscreenpager;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.HorizontalScrollView;
import android.widget.TextView;

/**
 * Off-screen pager fixture (GR2 W4 as a minimal, framework-only app).
 *
 * <p>Two pages sit side by side in a {@link HorizontalScrollView}. Each page is
 * exactly one display width, so page two is warm-mounted at {@code x = +width}
 * with every node attached and {@code View.VISIBLE} — precisely the Gaten
 * Analytics page that made {@code wait_for {text: "Accuracy"}} pass in 13 ms on
 * content the user could not see.
 *
 * <p>Observable behaviour:
 * <ul>
 *   <li>starting state: "Page one" is on screen; "Hidden" (page two) is attached
 *       but off screen, so it must be reported {@code visible: false} and must
 *       not be matched by a selector;</li>
 *   <li>"Next" scrolls the pager one page width, bringing "Hidden" on screen
 *       (the positive control: the same node becomes visible and tappable);</li>
 *   <li>"Back" scrolls back.</li>
 * </ul>
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosOffscreenPager";

    private HorizontalScrollView pager;
    private int pageWidth;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        pager = (HorizontalScrollView) findViewById(R.id.pager);
        pageWidth = getResources().getDisplayMetrics().widthPixels;
        setPageWidth(findViewById(R.id.page_one));
        setPageWidth(findViewById(R.id.page_two));
        ((Button) findViewById(R.id.next)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                pager.smoothScrollTo(pageWidth, 0);
                Log.i(TAG, "next scroll_to=" + pageWidth);
            }
        });
        ((Button) findViewById(R.id.back)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                pager.smoothScrollTo(0, 0);
                Log.i(TAG, "back scroll_to=0");
            }
        });
    }

    /** Give a page the display's width so the next page starts off screen. */
    private void setPageWidth(View page) {
        ViewGroup.LayoutParams params = page.getLayoutParams();
        params.width = pageWidth;
        page.setLayoutParams(params);
    }
}
