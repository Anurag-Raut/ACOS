public class BenchString {
    public static void main(String[] args) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < 1000000; i++) {
            sb.append(i);
            sb.append(',');
        }
        System.out.println("len=" + sb.length() + " checksum=" + sb.toString().hashCode());
    }
}
