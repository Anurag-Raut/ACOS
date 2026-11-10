// Microbenchmark: static call overhead (ns per invoke-static + return).
public class BenchStaticCall {
    static int f(int x) {
        return x + 1;
    }

    public static void main(String[] args) {
        int s = 0;
        for (int i = 0; i < 50000000; i++) {
            s = f(s);
        }
        System.out.println("checksum=" + s);
    }
}
