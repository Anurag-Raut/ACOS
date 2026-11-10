public class Main {
    static class Counter {
        static int total;
        static final int START;
        static {
            START = 10;
            total = START;
            System.out.println("static-init Counter START=" + START);
        }

        int value;
        String name;

        {
            System.out.println("instance-init");
        }

        Counter(String name) {
            this(name, 0);
            System.out.println("ctor1 " + name);
        }

        Counter(String name, int value) {
            this.name = name;
            this.value = value;
            total += value;
            System.out.println("ctor2 " + name + "=" + value);
        }

        int inc() {
            return ++value;
        }

        @Override
        public String toString() {
            return name + "(" + value + ")";
        }
    }

    static class Point {
        final int x, y;

        Point(int x, int y) {
            this.x = x;
            this.y = y;
        }

        int sum() {
            return x + y;
        }
    }

    public static void main(String[] args) {
        System.out.println("total.before=" + Counter.total);
        Counter a = new Counter("a");
        Counter b = new Counter("b", 5);
        System.out.println("a=" + a + " b=" + b);
        System.out.println("a.inc=" + a.inc() + " a.inc=" + a.inc());
        System.out.println("b.inc=" + b.inc());
        System.out.println("total.after=" + Counter.total);
        Point p = new Point(3, 4);
        Point q = new Point(-1, 2);
        System.out.println("p.sum=" + p.sum() + " q.sum=" + q.sum());
        System.out.println("p.x=" + p.x + " p.y=" + p.y);
        String s1 = "hello";
        String s2 = new String("hello");
        System.out.println("s1==s2=" + (s1 == s2));
        System.out.println("s1.equals(s2)=" + s1.equals(s2));
        System.out.println("identity=" + (a == a) + " " + (p == q));
        System.out.println("class.name=" + p.getClass().getName());
        System.out.println("default.object_prefix=" + new Object().toString().startsWith("java.lang.Object@"));
    }
}
