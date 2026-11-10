/**
 * Exception-floor regression (bootstrap corelib).
 *
 * <p>An exception thrown in a {@code <clinit>} must unwind only to the
 * innermost synchronous Rust→Java call base (the interpreter's
 * {@code initialize_class}), then be rethrown into the outer Java frames at
 * the right handler. An earlier implementation unwound past that Rust frame
 * and resumed with a stale frame index.
 *
 * <p>The native-initiated variant ({@code Class.forName}) lives in the
 * {@code acos-runtime} suite, which has the real core library.
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

    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    public static void main(String[] args) throws Exception {
        // 1. Direct Java access: the outer catch gets ExceptionInInitializerError
        //    whose cause is the original throwable.
        try {
            int v = Boom.VALUE;
            throw new AssertionError("Boom.VALUE must throw, got " + v);
        } catch (ExceptionInInitializerError e) {
            check(e.getCause() instanceof RuntimeException, "cause type");
            check("boom".equals(e.getCause().getMessage()), "cause message");
            System.out.println("eie:" + e.getCause().getMessage());
        }

        // 2. The class is now erroneous: a second access throws NoClassDefFoundError.
        try {
            int v = Boom.VALUE;
            throw new AssertionError("second access must throw, got " + v);
        } catch (NoClassDefFoundError e) {
            System.out.println("ncdfe");
        }

        System.out.println("done");
    }
}
