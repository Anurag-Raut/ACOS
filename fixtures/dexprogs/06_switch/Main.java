public class Main {
    static String packed(int x) {
        switch (x) {
            case 0: return "zero";
            case 1: return "one";
            case 2: return "two";
            case 3: return "three";
            case 4: return "four";
            default: return "other";
        }
    }

    static int sparse(int x) {
        switch (x) {
            case 1: return 10;
            case 100: return 20;
            case 1000: return 30;
            case 10000: return 40;
            case -5: return 50;
            default: return -1;
        }
    }

    static String str(String s) {
        switch (s) {
            case "alpha": return "A";
            case "beta": return "B";
            case "gamma": return "C";
            default: return "?";
        }
    }

    static int fall(int x) {
        int r = 0;
        switch (x) {
            case 1: r += 1;
            case 2: r += 2;
            case 3: r += 3; break;
            case 4: r += 4;
            default: r += 100;
        }
        return r;
    }

    public static void main(String[] args) {
        for (int i = -1; i <= 5; i++) {
            System.out.println("packed" + i + "=" + packed(i));
        }
        int[] xs = {1, 100, 1000, 10000, -5, 7};
        for (int x : xs) {
            System.out.println("sparse" + x + "=" + sparse(x));
        }
        String[] ss = {"alpha", "beta", "gamma", "delta"};
        for (String s : ss) {
            System.out.println("str_" + s + "=" + str(s));
        }
        for (int i = 1; i <= 5; i++) {
            System.out.println("fall" + i + "=" + fall(i));
        }
    }
}
