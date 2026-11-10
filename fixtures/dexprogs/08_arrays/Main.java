public class Main {
    public static void main(String[] args) {
        int[] ints = {1, 2, 3, 5, 8, 13};
        long[] longs = {10L, 20L, 30L};
        byte[] bytes = {0, 1, -1, 127, -128};
        char[] chars = {'A', 'b', '3', '\u00e9'};
        boolean[] bools = {true, false, true};
        String[] objects = {"x", "y", "z"};
        System.out.println("ints.len=" + ints.length);
        int isum = 0;
        for (int v : ints) isum += v;
        System.out.println("ints.sum=" + isum);
        System.out.println("ints[0]=" + ints[0] + " ints[5]=" + ints[5]);
        System.out.println("longs[2]=" + longs[2]);
        long lsum = 0;
        for (long v : longs) lsum += v;
        System.out.println("longs.sum=" + lsum);
        byte bsum = 0;
        for (byte v : bytes) bsum += v;
        System.out.println("bytes.sum=" + bsum);
        StringBuilder sb = new StringBuilder("chars=");
        for (char c : chars) {
            sb.append(c);
            sb.append(':');
            sb.append((int) c);
            sb.append(' ');
        }
        System.out.println(sb);
        System.out.println("bools=" + bools[0] + "," + bools[1] + "," + bools[2]);
        System.out.println("objects=" + objects[0] + objects[1] + objects[2]);
        int[][] grid = new int[3][4];
        for (int i = 0; i < grid.length; i++) {
            for (int j = 0; j < grid[i].length; j++) {
                grid[i][j] = i * 10 + j;
            }
        }
        System.out.println("grid.rows=" + grid.length + " grid[0].len=" + grid[0].length);
        System.out.println("grid[2][3]=" + grid[2][3]);
        int[][] jagged = {{1}, {1, 2}, {1, 2, 3}};
        int total = 0;
        for (int[] row : jagged) {
            for (int v : row) total += v;
        }
        System.out.println("jagged.total=" + total);
        char[][] letters = {{'a', 'b'}, {'c', 'd'}};
        System.out.println("letters=" + letters[0][0] + letters[0][1] + letters[1][0] + letters[1][1]);
        String[] filled = new String[3];
        System.out.println("filled0=" + filled[0]);
        filled[1] = "set";
        System.out.println("filled1=" + filled[1]);
        double[] dbl = new double[2];
        System.out.println("dbl0=" + dbl[0]);
        int[][] ragged = new int[2][];
        ragged[0] = new int[]{7, 8};
        ragged[1] = new int[]{9};
        System.out.println("ragged=" + ragged[1][0]);
        try {
            int bad = ints[6];
            System.out.println("bad=" + bad);
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("caught=" + e.getClass().getName());
        }
        String[] arr = new String[0];
        System.out.println("empty.len=" + arr.length);
        Object[] cov = new String[]{"a", "b"};
        System.out.println("cov.len=" + cov.length);
    }
}
