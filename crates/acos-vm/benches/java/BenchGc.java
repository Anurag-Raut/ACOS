// GC microbenchmark: build a ~100 MB live set, then allocate garbage to force
// collections while the live set is large. `gc_ms / gc_count` from the metrics
// is the average GC pause.
import java.util.ArrayList;

public class BenchGc {
    public static void main(String[] args) {
        ArrayList<int[]> live = new ArrayList<>();
        for (int i = 0; i < 25600; i++) {
            live.add(new int[1024]);
        }
        long s = 0;
        for (int i = 0; i < 4000000; i++) {
            int[] t = new int[16 + (i & 7)];
            t[0] = i;
            s += t[0];
        }
        System.out.println("live=" + live.size() + " s=" + s);
    }
}
