import java.util.ArrayList;
import java.util.List;
import java.util.function.BiFunction;
import java.util.function.Function;
import java.util.function.IntBinaryOperator;
import java.util.function.Supplier;

public class MethodRefs {
    public static int square(int x) {
        return x * x;
    }

    public static String prefix(String s) {
        return "p:" + s;
    }

    public int instanceAdd(int a, int b) {
        return a + b + 1;
    }

    public static int apply(Function<Integer, Integer> f, int x) {
        return f.apply(x);
    }

    public static int applyInt(IntBinaryOperator op, int a, int b) {
        return op.applyAsInt(a, b);
    }

    public static String supply(Supplier<String> s) {
        return s.get();
    }

    public static int bi(BiFunction<Integer, Integer, Integer> f, int a, int b) {
        return f.apply(a, b);
    }

    public static void main(String[] args) {
        List<Integer> xs = new ArrayList<>();
        for (int i = 0; i < 5; i++) xs.add(i);
        int s = 0;
        for (int x : xs) {
            Function<Integer, Integer> f = y -> y + x;
            s += f.apply(x);
        }
        MethodRefs mr = new MethodRefs();
        s += apply(MethodRefs::square, 7);
        s += applyInt(Integer::sum, 3, 4);
        s += applyInt((a, b) -> a * b, 2, 5);
        s += bi(mr::instanceAdd, 1, 2);
        s += supply(() -> prefix("x")).length();
        s += supply(() -> "y").length();
        System.out.println(s);
    }
}
