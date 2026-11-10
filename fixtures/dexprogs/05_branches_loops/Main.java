public class Main {
    static int collatzSteps(int n) {
        int steps = 0;
        while (n != 1) {
            if ((n & 1) == 0) {
                n = n / 2;
            } else {
                n = 3 * n + 1;
            }
            steps++;
        }
        return steps;
    }

    public static void main(String[] args) {
        System.out.println("collatz27=" + collatzSteps(27));
        int sum = 0;
        for (int i = 0; i < 10; i++) {
            sum += i;
        }
        System.out.println("sum0to9=" + sum);
        int evens = 0;
        for (int i = 0; i <= 20; i++) {
            if (i % 2 != 0) continue;
            evens++;
        }
        System.out.println("evens=" + evens);
        int prod = 1;
        int n = 5;
        do {
            prod *= n;
            n--;
        } while (n > 0);
        System.out.println("fact5=" + prod);
        int found = -1;
        outer:
        for (int i = 0; i < 5; i++) {
            for (int j = 0; j < 5; j++) {
                if (i * j == 6) {
                    found = i * 10 + j;
                    break outer;
                }
            }
        }
        System.out.println("labeled=" + found);
        int count = 0;
        for (int i = 0; i < 100; i++) {
            if (i > 50) break;
            if (i % 3 == 0) count += i;
        }
        System.out.println("sumdiv3_le50=" + count);
        String grade;
        int score = 87;
        if (score >= 90) grade = "A";
        else if (score >= 80) grade = "B";
        else if (score >= 70) grade = "C";
        else grade = "F";
        System.out.println("grade=" + grade);
        System.out.println("ternary=" + (score >= 60 ? "pass" : "fail"));
        int x = 5;
        System.out.println("and_short=" + (x > 0 && x < 10));
        System.out.println("or_short=" + (x < 0 || x == 5));
        System.out.println("not=" + !(x == 5));
        int[] fibs = new int[12];
        int a = 0, b = 1;
        for (int i = 0; i < fibs.length; i++) {
            fibs[i] = a;
            int t = a + b;
            a = b;
            b = t;
        }
        StringBuilder sb = new StringBuilder("fibs=");
        for (int i = 0; i < fibs.length; i++) {
            if (i > 0) sb.append(',');
            sb.append(fibs[i]);
        }
        System.out.println(sb);
        int halvings = 0;
        for (int i = 100; i > 0; i /= 2) {
            halvings++;
        }
        System.out.println("halvings=" + halvings);
        int loops = 0;
        while (true) {
            loops++;
            if (loops == 7) break;
        }
        System.out.println("loops=" + loops);
        System.out.println("neg_mod=" + (-7 % 3));
        System.out.println("neg_div=" + (-7 / 3));
        System.out.println("zero_ok=" + (x != 0 ? 100 / x : -1));
    }
}
