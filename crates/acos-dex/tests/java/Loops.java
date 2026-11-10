public class Loops {
    public static int sumFor(int n) {
        int s = 0;
        for (int i = 0; i < n; i++) {
            s += i;
        }
        return s;
    }

    public static int sumWhile(int n) {
        int s = 0;
        int i = 0;
        while (i < n) {
            s += i * 2;
            i++;
        }
        return s;
    }

    public static int sumDo(int n) {
        int s = 0;
        int i = 0;
        do {
            s += i;
            i++;
        } while (i < n);
        return s;
    }

    public static int nested(int n) {
        int s = 0;
        for (int i = 0; i < n; i++) {
            for (int j = i; j < n; j++) {
                if ((i + j) % 3 == 0) {
                    continue;
                }
                if (i * j > 100) {
                    break;
                }
                s += i ^ j;
            }
        }
        return s;
    }

    public static int labeled(int n) {
        int s = 0;
        outer:
        for (int i = 0; i < n; i++) {
            for (int j = 0; j < n; j++) {
                if (i * j > n) {
                    break outer;
                }
                s += i - j;
            }
        }
        return s;
    }

    public static void main(String[] args) {
        System.out.println(sumFor(10) + sumWhile(10) + sumDo(10) + nested(10) + labeled(10));
    }
}
