// Microbenchmark: pure frame-local arithmetic + branches, no arrays/calls.
// Used to measure the interpreter dispatch floor (ns/instruction).
public class BenchArith {
    public static void main(String[] args) {
        int a = 1, b = 2, c = 3, d = 4;
        long s = 0;
        for (int i = 0; i < 50000000; i++) {
            a = a + b;
            b = b ^ c;
            c = c + d;
            d = d - a;
            s = s + a;
            s = s + b;
        }
        System.out.println("checksum=" + (s + a + b + c + d));
    }
}
