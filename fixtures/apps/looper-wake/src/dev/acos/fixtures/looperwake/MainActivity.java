package dev.acos.fixtures.looperwake;

import android.app.Activity;
import android.os.Bundle;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.Looper;
import android.os.SystemClock;
import android.util.Log;
import android.view.View;
import android.view.ViewTreeObserver;
import android.widget.Button;
import android.widget.TextView;

/**
 * LOOPERWAKE fixture: an <b>idle</b> Activity whose state is changed by a
 * non-main thread.
 *
 * <p>On a real device a handler post from a background thread is a
 * {@code MessageQueue.enqueueMessage -> nativeWake} (an {@code eventfd} write)
 * that interrupts the main {@code Looper.pollOnce} {@code epoll_wait}; the
 * message runs at once and the view's {@code invalidate} schedules a traversal
 * through {@code ViewRootImpl.scheduleTraversals -> Choreographer.scheduleVsync}
 * (a vsync event on the DisplayEventReceiver's BitTube wakes the same looper
 * again). Nothing about that needs a touch, a key or any other input.
 *
 * <p>Three paths, all started without input:
 *
 * <ol>
 *   <li><b>plain Thread + runOnUiThread</b> (the reported shape): a plain Java
 *       thread sleeps {@link #POST_DELAY_MS} after the first frame, then calls
 *       {@code runOnUiThread} to {@code setText("bg-update")}. Logs
 *       {@code bg-post} (when the thread posts) then {@code ui-drain} (when the
 *       main looper runs it) and {@code ui-setText}.</li>
 *   <li><b>HandlerThread callback + main Handler</b>: a non-main thread that
 *       owns its own {@code Looper} (the shape of a binder-thread callback: a
 *       framework thread, not the UI thread, deciding to update the UI) posts to
 *       the main Handler. Logs {@code bg2-post} / {@code ui2-drain}.</li>
 *   <li><b>postInvalidate from a plain thread</b>: no looper message at all in
 *       the app's own code — the traversal request is made from the background
 *       thread, so the frame can only appear if {@code scheduleVsync} wakes the
 *       frame pump. {@link PulseView} logs each real draw.</li>
 * </ol>
 *
 * <p>The first two also work on demand from the {@code go} / {@code invalidate}
 * buttons, so a test can compare the "input happened" case with the idle case.
 * Every mark is logged with tag {@code AcosLooperWake}.
 */
public class MainActivity extends Activity {
    public static final String TAG = "AcosLooperWake";

    /**
     * Delay from the first drawn frame to the background post.
     *
     * <p>Measured from the first frame, not from {@code onCreate}: an instrument
     * that fires during launch would be absorbed by the launch settle and prove
     * nothing. 1500 ms leaves the launch reply (which binds the first settled
     * frame) and the pre-state assertion comfortably ahead of the update, so the
     * following assertion really waits on an idle main looper.
     */
    static final long POST_DELAY_MS = 1500;

    private TextView statusView;
    private TextView drawsView;
    private PulseView pulseView;
    private final Handler main = new Handler(Looper.getMainLooper());
    private HandlerThread backgroundThread;
    private Handler backgroundHandler;
    private int frames;
    private boolean launchPostArmed;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        statusView = (TextView) findViewById(R.id.status);
        drawsView = (TextView) findViewById(R.id.draws);
        pulseView = (PulseView) findViewById(R.id.pulse);

        // The first drawn frame arms the background post (see POST_DELAY_MS).
        pulseView.getViewTreeObserver().addOnPreDrawListener(
                new ViewTreeObserver.OnPreDrawListener() {
                    @Override
                    public boolean onPreDraw() {
                        if (!launchPostArmed) {
                            launchPostArmed = true;
                            armBackgroundPost();
                        }
                        return true;
                    }
                });

        // Path 2 (on demand): a HandlerThread (a non-main Looper thread) posts
        // to the main Handler 300 ms later.
        ((Button) findViewById(R.id.go)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Log.i(TAG, "go.click");
                startBackgroundHandlerPost();
            }
        });

        // Path 3 (on demand): a plain thread calls View.postInvalidate() — the
        // traversal request itself comes from a non-main thread.
        ((Button) findViewById(R.id.invalidate)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Log.i(TAG, "invalidate.click");
                startBackgroundInvalidate(300);
            }
        });
    }

    /** Path 1: plain thread, {@link Activity#runOnUiThread}. */
    private void armBackgroundPost() {
        frames++;
        Log.i(TAG, "first-draw frame=" + frames + " t=" + SystemClock.uptimeMillis());
        final long armedAt = SystemClock.uptimeMillis();
        Thread thread = new Thread(new Runnable() {
            @Override
            public void run() {
                sleepUntil(armedAt + POST_DELAY_MS);
                Log.i(TAG, "bg-post t=" + SystemClock.uptimeMillis());
                final long postedAt = SystemClock.uptimeMillis();
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        // The guest's own post -> drain latency: the number a
                        // broken wake inflates to "the next input".
                        Log.i(TAG, "ui-drain latency_ms="
                                + (SystemClock.uptimeMillis() - postedAt)
                                + " t=" + SystemClock.uptimeMillis());
                        statusView.setText(R.string.bg_update);
                        Log.i(TAG, "ui-setText t=" + SystemClock.uptimeMillis());
                    }
                });
                // Path 3 follows the text change: a background postInvalidate
                // must produce a drawn frame of its own.
                sleepUntil(SystemClock.uptimeMillis() + 250);
                Log.i(TAG, "bg-invalidate t=" + SystemClock.uptimeMillis());
                PulseView.sBgInvalidateAtMs = SystemClock.uptimeMillis();
                pulseView.postInvalidate();
            }
        }, "acos-bg-post");
        thread.setDaemon(true);
        thread.start();
    }

    /** Path 2: a non-main thread with its own Looper posts to the main Handler. */
    private void startBackgroundHandlerPost() {
        if (backgroundThread == null) {
            backgroundThread = new HandlerThread("acos-bg-handler");
            backgroundThread.start();
            backgroundHandler = new Handler(backgroundThread.getLooper());
        }
        backgroundHandler.post(new Runnable() {
            @Override
            public void run() {
                Log.i(TAG, "bg2-post t=" + SystemClock.uptimeMillis());
                final long postedAt = SystemClock.uptimeMillis();
                main.post(new Runnable() {
                    @Override
                    public void run() {
                        Log.i(TAG, "ui2-drain latency_ms="
                                + (SystemClock.uptimeMillis() - postedAt)
                                + " t=" + SystemClock.uptimeMillis());
                        statusView.setText(R.string.go_update);
                        Log.i(TAG, "ui2-setText t=" + SystemClock.uptimeMillis());
                    }
                });
            }
        });
    }

    /** Path 3 (on demand): a plain thread asks the view for a traversal. */
    private void startBackgroundInvalidate(final long delayMs) {
        final long at = SystemClock.uptimeMillis() + delayMs;
        Thread thread = new Thread(new Runnable() {
            @Override
            public void run() {
                sleepUntil(at);
                Log.i(TAG, "bg-invalidate-on-demand t=" + SystemClock.uptimeMillis());
                PulseView.sBgInvalidateAtMs = SystemClock.uptimeMillis();
                pulseView.postInvalidate();
            }
        }, "acos-bg-invalidate");
        thread.setDaemon(true);
        thread.start();
    }

    private static void sleepUntil(long uptimeMs) {
        long remaining = uptimeMs - SystemClock.uptimeMillis();
        while (remaining > 0) {
            try {
                Thread.sleep(remaining);
            } catch (InterruptedException ignored) {
                return;
            }
            remaining = uptimeMs - SystemClock.uptimeMillis();
        }
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        if (backgroundThread != null) {
            backgroundThread.quitSafely();
        }
    }
}
