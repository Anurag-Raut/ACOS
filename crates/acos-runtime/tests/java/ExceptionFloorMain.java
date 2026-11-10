/**
 * Exception-floor regression (real Android 14 core library).
 *
 * <p>Exercises the native-initiated variant missing from the bootstrap
 * corelib: {@code Class.forName} is a Java method that calls the
 * {@code classForName} native, which calls {@code initialize_class} while a
 * Java frame is only reachable across the native call. A {@code <clinit>}
 * failure there must stop at the native's Rust base and be rethrown into the
 * outer Java handler as {@code ExceptionInInitializerError} — not unwind past
 * the native frame and resume with a stale frame index.
 */
public final class ExceptionFloorMain {
    static boolean fail() {
        return true;
    }

    /** Its {@code <clinit>} throws; the first access must report EIE. */
    static final class Boom {
        static final int VALUE;
        static {
            if (fail()) {
                throw new RuntimeException("boom");
            }
            VALUE = 1;
        }
    }

    /** A fresh pair so the native-initiated test is not masked by step 1. */
    static final class Boom2 {
        static final int VALUE;
        static {
            if (fail()) {
                throw new IllegalStateException("boom2");
            }
            VALUE = 2;
        }
    }

    static final class Nested2 {
        static final int VALUE = Boom2.VALUE;
    }

    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    public static void main(String[] args) throws Exception {
        // 1. Direct Java access.
        try {
            int v = Boom.VALUE;
            throw new AssertionError("Boom.VALUE must throw, got " + v);
        } catch (ExceptionInInitializerError e) {
            check(e.getCause() instanceof RuntimeException, "cause type");
            check("boom".equals(e.getCause().getMessage()), "cause message");
            System.out.println("eie:" + e.getCause().getMessage());
        }

        // 2. Second access to an erroneous class.
        try {
            int v = Boom.VALUE;
            throw new AssertionError("second access must throw, got " + v);
        } catch (NoClassDefFoundError e) {
            System.out.println("ncdfe");
        }

        // 3. Native-initiated init (Class.forName -> classForName native ->
        //    initialize_class) with a nested <clinit> failure.
        try {
            Class.forName("ExceptionFloorMain$Nested2");
            throw new AssertionError("Class.forName(Nested2) must throw");
        } catch (ExceptionInInitializerError e) {
            check(e.getCause() instanceof IllegalStateException, "forname cause type");
            check("boom2".equals(e.getCause().getMessage()), "forname cause message");
            System.out.println("forname-eie:" + e.getCause().getMessage());
        }

        // 4. And the erroneous class stays erroneous after the native path.
        try {
            int v = Boom2.VALUE;
            throw new AssertionError("Boom2 second access must throw, got " + v);
        } catch (NoClassDefFoundError e) {
            System.out.println("ncdfe2");
        }

        System.out.println("done");
    }
}
