// Microbenchmark: primitive array get/put only (ns/instruction for the array
// opcode family).
public class BenchArray {
    public static void main(String[] args) {
        int[] arr = new int[1024];
        long s = 0;
        for (int i = 0; i < 50000000; i++) {
            int j = i & 1023;
            arr[j] = i;
            s = s + arr[j];
        }
        System.out.println("checksum=" + s);
    }
}
