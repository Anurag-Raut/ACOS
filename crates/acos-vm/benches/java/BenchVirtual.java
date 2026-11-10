// Framework-shaped: a polymorphic (interface) call site with three receiver
// classes, exercising invoke-interface dispatch and inline-cache behaviour.
public class BenchVirtual {
    interface Shape {
        int area(int x);
    }

    static class A implements Shape {
        public int area(int x) { return x + 1; }
    }

    static class B implements Shape {
        public int area(int x) { return x * 3; }
    }

    static class C implements Shape {
        public int area(int x) { return x - 7; }
    }

    public static void main(String[] args) {
        Shape[] shapes = new Shape[] { new A(), new B(), new C() };
        long s = 0;
        for (int i = 0; i < 20000000; i++) {
            s += shapes[i % 3].area(i);
        }
        System.out.println("checksum=" + s);
    }
}
