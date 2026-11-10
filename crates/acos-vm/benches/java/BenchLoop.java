public class BenchLoop {
    public static void main(String[] args) {
        int[] arr = new int[1024];
        long s = 0;
        for (int i = 0; i < 50000000; i++) {
            arr[i & 1023] = i;
            s += arr[(i * 7) & 1023];
        }
        System.out.println("checksum=" + s);
    }
}
