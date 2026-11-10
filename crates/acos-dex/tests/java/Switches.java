public class Switches {
    public static int packed(int x) {
        switch (x) {
            case 0: return 10;
            case 1: return 11;
            case 2: return 12;
            case 3: return 13;
            case 4: return 14;
            default: return -1;
        }
    }

    public static int sparse(int x) {
        switch (x) {
            case 1: return 100;
            case 100: return 200;
            case 1000: return 300;
            case 10000: return 400;
            case -5: return 500;
            default: return 0;
        }
    }

    public static int mixed(int x) {
        switch (x) {
            case 2: return 1;
            case 4: return 2;
            case 8: return 4;
            case 16: return 8;
            case 32: return 16;
            default: return 0;
        }
    }

    public static int onString(String s) {
        switch (s) {
            case "alpha": return 1;
            case "beta": return 2;
            case "gamma": return 3;
            default: return 0;
        }
    }

    public static void main(String[] args) {
        System.out.println(packed(3) + sparse(1000) + mixed(8)
                + onString("beta") + onString("delta"));
    }
}
