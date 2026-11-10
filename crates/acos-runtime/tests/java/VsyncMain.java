// Java-level acceptance for T-30: DisplayEventReceiver + Choreographer vsync.
//
// Uses the real `Choreographer.postFrameCallback` sequence on a real `Looper`.
// The vsync source is the deterministic 60 Hz virtual generator in
// `natives/fwk_vsync.rs`: a tick exists only when a receiver called
// `nativeScheduleVsync`, and each tick advances the frame time by one interval.
//
// Two modes:
//   * default: post 3 frame callbacks and quit after the third; prints the frame
//     times and whether they are strictly monotonic.
//   * "none": create the Choreographer but schedule nothing; the looper must run
//     out its 120 ms timer with zero frame callbacks (idle source, no tick).
import android.os.Handler;
import android.os.Looper;
import android.view.Choreographer;

public class VsyncMain {
    static int frames = 0;
    static long[] times = new long[4];

    public static void main(String[] args) {
        Looper.prepare();
        final Choreographer choreographer = Choreographer.getInstance();
        final Looper looper = Looper.myLooper();
        boolean none = args.length > 0 && args[0].equals("none");

        if (none) {
            new Handler(looper).postDelayed(new Runnable() {
                @Override
                public void run() {
                    looper.quitSafely();
                }
            }, 120);
            Looper.loop();
            System.out.println("frames=" + frames);
            System.out.println("done");
            return;
        }

        final Choreographer.FrameCallback[] self = new Choreographer.FrameCallback[1];
        self[0] = new Choreographer.FrameCallback() {
            @Override
            public void doFrame(long frameTimeNanos) {
                if (frames < times.length) {
                    times[frames] = frameTimeNanos;
                }
                frames++;
                if (frames < 3) {
                    choreographer.postFrameCallback(self[0]);
                } else {
                    looper.quitSafely();
                }
            }
        };
        choreographer.postFrameCallback(self[0]);

        // Safety net: never let the looper hang.
        new Handler(looper).postDelayed(new Runnable() {
            @Override
            public void run() {
                looper.quitSafely();
            }
        }, 3000);

        Looper.loop();

        System.out.println("frames=" + frames);
        for (int i = 0; i < times.length; i++) {
            System.out.println("time" + i + "=" + times[i]);
        }
        boolean monotonic = frames == 3
                && times[0] > 0
                && times[1] > times[0]
                && times[2] > times[1];
        System.out.println("monotonic=" + monotonic);
        System.out.println("done");
    }
}
