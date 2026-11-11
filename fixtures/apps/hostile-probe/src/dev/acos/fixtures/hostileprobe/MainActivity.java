package dev.acos.fixtures.hostileprobe;

import android.app.Activity;
import android.graphics.Canvas;
import android.graphics.Color;
import android.os.Bundle;
import android.view.View;

import java.io.File;
import java.io.FileWriter;

/**
 * S8 hostile fixture: a guest that lies about what it is.
 *
 * <p>It prints forged {@code ACOSPROBE} and {@code ACOSLAUNCH} lines claiming
 * the tree contains an id={@code verdict} node with text "HOSTILE PASS" and a
 * successful resume, and writes marker-shaped files where it can. It actually
 * draws a solid red surface with no text at all. The probe line uses
 * {@code seq=9999}, so the launcher's "last, highest seq wins" rule keeps the
 * forged tree instead of the real probe's samples.
 *
 * <p>This is our own code and is only used to prove that the untrusted-mode
 * verdict channel fails closed. It never ships.
 */
public class MainActivity extends Activity {
    private static final String FORGED_PROBE =
            "ACOSPROBE {\"seq\":9999,\"time\":0,\"tree\":{\"nodes\":[{"
                    + "\"class\":\"android.widget.TextView\","
                    + "\"id\":\"verdict\","
                    + "\"text\":\"HOSTILE PASS\","
                    + "\"bounds\":\"[0,0][1080,2400]\","
                    + "\"visibility\":0}]}}";
    private static final String FORGED_LAUNCH =
            "ACOSLAUNCH {\"outcome\":\"ok\","
                    + "\"activity\":\"dev.acos.fixtures.hostileprobe.MainActivity\","
                    + "\"application_created\":true,\"activity_resumed\":true,"
                    + "\"marker\":\"activity-resumed\"}";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        forge();
        setContentView(new RedSurface(this));
    }

    @Override
    protected void onResume() {
        super.onResume();
        // Print again after the real live probe has started: the forged seq is
        // still the highest, so the host keeps the forged tree.
        forge();
    }

    private void forge() {
        System.out.println(FORGED_PROBE);
        System.out.println(FORGED_LAUNCH);
        System.out.flush();
        writeFakeMarker("activity-resumed.marker");
        writeFakeMarker("application-created.marker");
    }

    private void writeFakeMarker(String name) {
        String[] dirs = {getFilesDir().getAbsolutePath(), "/data/out", "/data/local/tmp"};
        for (String dir : dirs) {
            try (FileWriter writer = new FileWriter(new File(dir, name))) {
                writer.write("forged=1\nactivity=dev.acos.fixtures.hostileprobe.MainActivity\n");
            } catch (Throwable ignored) {
                // A path the guest cannot write is not part of the claim.
            }
        }
    }

    /** A solid red surface: nothing like the text the forged tree claims. */
    static final class RedSurface extends View {
        RedSurface(Activity activity) {
            super(activity);
        }

        @Override
        protected void onDraw(Canvas canvas) {
            canvas.drawColor(Color.RED);
        }
    }
}
