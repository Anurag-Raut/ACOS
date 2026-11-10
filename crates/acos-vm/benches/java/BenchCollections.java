// Framework-shaped: ArrayList + HashMap + StringBuilder traffic, i.e. the
// boxing/native-call/collection pattern that dominates framework code.
//
// Note: the ACOS core library backs HashMap with a small linear-scan store
// (correctness-first), so the map is intentionally small here; the point is to
// stress invoke/boxing/collection opcode shapes, not hash-table asymptotics.
import java.util.ArrayList;
import java.util.HashMap;

public class BenchCollections {
    public static void main(String[] args) {
        int n = 500;
        int rounds = 100;
        ArrayList<Integer> list = new ArrayList<>();
        HashMap<Integer, Integer> map = new HashMap<>();
        for (int i = 0; i < n; i++) {
            list.add(i);
            map.put(i, i * 2);
        }
        long s = 0;
        int lastLen = 0;
        for (int round = 0; round < rounds; round++) {
            StringBuilder sb = new StringBuilder();
            for (int i = 0; i < n; i++) {
                s += list.get(i);
                Integer got = map.get(i);
                if (got != null) {
                    s += got;
                }
                sb.append(i);
                sb.append(',');
            }
            lastLen = sb.length();
        }
        System.out.println("checksum=" + s + " len=" + lastLen);
    }
}
