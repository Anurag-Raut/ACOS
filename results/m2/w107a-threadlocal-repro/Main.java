import java.util.concurrent.atomic.AtomicInteger;

public class Main {
    static class LookupStack {
        final int depth;

        LookupStack(int depth) {
            this.depth = depth;
        }

        @Override
        public String toString() {
            return "LookupStack(" + depth + ")";
        }
    }

    static final AtomicInteger supplierCalls = new AtomicInteger();

    public static void main(String[] args) {
        // 1. ThreadLocal.withInitial(...).get() must run the supplier exactly
        //    once and return a non-null value (the ResourcesImpl.mLookupStack
        //    pattern: `ThreadLocal.withInitial(() -> new LookupStack())`).
        ThreadLocal<LookupStack> stack = ThreadLocal.withInitial(() -> {
            supplierCalls.incrementAndGet();
            return new LookupStack(7);
        });
        LookupStack first = stack.get();
        System.out.println("suppliedNonNull=" + (first != null));
        System.out.println("supplied=" + first);
        System.out.println("supplierCalls=" + supplierCalls.get());

        // 2. Repeat get() returns the same stored instance.
        LookupStack second = stack.get();
        System.out.println("sameInstance=" + (first == second));
        System.out.println("supplierCallsAfter2=" + supplierCalls.get());

        // 3. A plain ThreadLocal with an overridden initialValue().
        ThreadLocal<Integer> plain = new ThreadLocal<Integer>() {
            @Override
            protected Integer initialValue() {
                return 41;
            }
        };
        System.out.println("plainInit=" + plain.get());

        // 4. set/get/remove/re-initialise.
        ThreadLocal<String> named = new ThreadLocal<String>();
        named.set("set-value");
        System.out.println("setGet=" + named.get());
        named.remove();
        System.out.println("afterRemove=" + named.get());

        // 5. currentThread() is the thread whose threadLocals field is used.
        Thread current = Thread.currentThread();
        System.out.println("currentThreadNonNull=" + (current != null));

        // 6. Null initial values are legal and must stay null.
        ThreadLocal<Object> nullInit = ThreadLocal.withInitial(() -> null);
        System.out.println("nullInit=" + nullInit.get());
    }
}
