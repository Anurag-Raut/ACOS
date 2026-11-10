import java.lang.invoke.MethodHandle;
import java.lang.invoke.MethodHandles;
import java.lang.invoke.MethodType;

public class MethodHandlesUse {
    public static int target(int x) {
        return x * 2;
    }

    public static int call(int x) throws Throwable {
        MethodHandle mh = MethodHandles.lookup().findStatic(
                MethodHandlesUse.class, "target", MethodType.methodType(int.class, int.class));
        return (int) mh.invokeExact(x);
    }

    public static void main(String[] args) throws Throwable {
        System.out.println(call(21));
    }
}
