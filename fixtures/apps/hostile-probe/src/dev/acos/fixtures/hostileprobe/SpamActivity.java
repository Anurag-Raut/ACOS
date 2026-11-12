package dev.acos.fixtures.hostileprobe;

import android.os.Bundle;

/**
 * W44 hostile fixture: probes the host's bounded probe-line reader.
 *
 * <p>One line is larger than the host's 8 MiB cap (it must be drained and
 * counted, never parsed or kept), and a burst of valid forged lines follows
 * (the highest seq still wins the tree). The content claims "HOSTILE PASS" in
 * a "probe-spam" node so an untrusted tree-only assertion has something to
 * (wrongly) accept if the fail-closed rule regresses.
 */
public class SpamActivity extends MainActivity {
    @Override
    void forge() {
        // This activity prints its own forged sample; do not out-rank it.
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        // > 8 MiB + newline: the host drops this whole line and counts it.
        StringBuilder huge = new StringBuilder(9 * 1024 * 1024);
        huge.append("ACOSPROBE {\"seq\":1,\"tree\":{\"nodes\":[],\"pad\":\"");
        while (huge.length() < 8 * 1024 * 1024 + 2048) {
            huge.append('x');
        }
        huge.append("\"}}");
        System.out.println(huge);
        // A burst of valid, under-cap forged lines; the last seq wins.
        for (int i = 0; i < 64; i++) {
            StringBuilder pad = new StringBuilder(32 * 1024);
            while (pad.length() < 32 * 1024) {
                pad.append('y');
            }
            System.out.println("ACOSPROBE {\"seq\":" + (1000 + i) + ",\"time\":0,"
                    + "\"windows\":[{\"window\":\"main\",\"z\":0}],"
                    + "\"tree\":{\"nodes\":[{"
                    + "\"class\":\"android.widget.TextView\","
                    + "\"id\":\"probe-spam\","
                    + "\"text\":\"HOSTILE PASS\","
                    + "\"window\":\"main\","
                    + "\"bounds\":\"[0,0][1080,2400]\","
                    + "\"visibility\":0}],"
                    + "\"window_count\":1,\"node_budget\":2000,\"truncated\":false,"
                    + "\"pad\":\"" + pad + "\"}}");
        }
        System.out.println(MainActivity.FORGED_LAUNCH);
        System.out.flush();
        setContentView(new RedSurface(this));
    }
}
