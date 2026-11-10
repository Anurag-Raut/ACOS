public class BenchFib {
    static long fib(int n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
    public static void main(String[] args) {
        System.out.println("checksum=" + fib(32));
    }
}
