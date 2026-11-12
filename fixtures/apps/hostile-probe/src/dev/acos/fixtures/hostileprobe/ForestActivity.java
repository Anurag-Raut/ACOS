package dev.acos.fixtures.hostileprobe;

import android.os.Bundle;

/**
 * W44 hostile fixture: a forged multi-window forest that says
 * {@code truncated:false} while omitting the node a scenario asks for.
 *
 * <p>The host must not turn that into {@code TREE_TRUNCATED} (it is not a
 * truncated tree) and on an untrusted run must not turn it into a pass; the
 * tree is guest-asserted and the missing node is a real
 * {@code ELEMENT_NOT_FOUND} on a trusted control only.
 */
public class ForestActivity extends MainActivity {
    @Override
    void forge() {
        // This activity prints its own forged sample; do not out-rank it.
    }

    private static final String FOREST =
            "ACOSPROBE {\"seq\":9999,\"time\":0,"
                    + "\"windows\":[{\"window\":\"main\",\"z\":0},{\"window\":\"ghost\",\"z\":1}],"
                    + "\"tree\":{\"nodes\":[{"
                    + "\"class\":\"android.widget.TextView\","
                    + "\"id\":\"main-only\","
                    + "\"text\":\"FORGED FOREST\","
                    + "\"window\":\"main\","
                    + "\"bounds\":\"[0,0][1080,2400]\","
                    + "\"visibility\":0}],"
                    + "\"window_count\":2,\"node_budget\":2000,\"truncated\":false}}";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        System.out.println(FOREST);
        System.out.println(MainActivity.FORGED_LAUNCH);
        System.out.flush();
        // Keep the inherited red surface; this activity asserts nothing visually.
        setContentView(new RedSurface(this));
    }
}
