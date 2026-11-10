import java.util.function.BiFunction;
import java.util.function.Function;
import java.util.function.Predicate;
import java.util.function.Supplier;

public class Main {
    static class Outer {
        private final int base;

        Outer(int base) {
            this.base = base;
        }

        class Inner {
            int add(int x) {
                return base + x;
            }
        }

        static class Nested {
            int twice(int x) {
                return 2 * x;
            }
        }

        Runnable makeRunnable() {
            return new Runnable() {
                @Override
                public void run() {
                    System.out.println("anon.run base=" + base);
                }
            };
        }
    }

    interface Op {
        int apply(int x);
    }

    static int applyOp(Op op, int x) {
        return op.apply(x);
    }

    static int square(int x) {
        return x * x;
    }

    public static void main(String[] args) {
        Outer outer = new Outer(10);
        Outer.Inner inner = outer.new Inner();
        System.out.println("inner.add=" + inner.add(5));
        System.out.println("nested.twice=" + new Outer.Nested().twice(21));
        outer.makeRunnable().run();
        Runnable anon = new Runnable() {
            @Override
            public void run() {
                System.out.println("top-level anon");
            }
        };
        anon.run();
        Op plusOne = new Op() {
            @Override
            public int apply(int x) {
                return x + 1;
            }
        };
        System.out.println("anonOp=" + plusOne.apply(41));
        Op lambda = x -> x * 3;
        System.out.println("lambda=" + lambda.apply(14));
        Op blockLambda = x -> {
            int y = x + 1;
            return y * y;
        };
        System.out.println("blockLambda=" + blockLambda.apply(4));
        System.out.println("applyOp=" + applyOp(x -> x - 1, 100));
        System.out.println("methodRef=" + applyOp(Main::square, 12));
        Supplier<String> supplier = () -> "supplied";
        System.out.println("supplier=" + supplier.get());
        Function<Integer, String> fn = x -> "n=" + x;
        System.out.println("function=" + fn.apply(7));
        BiFunction<Integer, Integer, Integer> sum = (a, b) -> a + b;
        System.out.println("sum=" + sum.apply(20, 22));
        int captured = 5;
        Op capture = x -> x + captured;
        System.out.println("capture=" + capture.apply(1));
        Function<String, Integer> len = String::length;
        System.out.println("len=" + len.apply("abcdef"));
        Predicate<Integer> positive = x -> x > 0;
        System.out.println("positive=" + positive.test(3) + " " + positive.test(-3));
        Runnable fromThread = () -> System.out.println("runnable done");
        fromThread.run();
    }
}
