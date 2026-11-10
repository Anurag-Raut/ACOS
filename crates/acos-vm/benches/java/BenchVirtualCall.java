// Microbenchmark: virtual call overhead (ns per invoke-virtual + return).
public class BenchVirtualCall {
    static class Impl {
        int f(int x) {
            return x + 1;
        }
    }

    public static void main(String[] args) {
        Impl o = new Impl();
        int s = 0;
        for (int i = 0; i < 50000000; i++) {
            s = o.f(s);
        }
        System.out.println("checksum=" + s);
    }
}
