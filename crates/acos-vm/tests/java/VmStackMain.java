/**
 * W95-A regression for the empty `Thread.getStackTrace()` (W88-A blocker 4).
 *
 * Kotlin's `Intrinsics.createParameterIsNullExceptionMessage` indexes
 * `stackTrace[0]` unconditionally. `VMStack.getThreadStackTrace` used to return
 * a zero-length array, so that index threw a spurious
 * `ArrayIndexOutOfBoundsException` instead of the intended
 * `NullPointerException`.
 */
public class VmStackMain {
    public static void main(String[] args) {
        StackTraceElement[] st = Thread.currentThread().getStackTrace();
        System.out.println(st.length > 0);
        if (st.length > 0) {
            String cls = st[0].getClassName();
            System.out.println(cls != null && !cls.isEmpty());
        }
        // The exact Kotlin shape: index [0] of a `Throwable.getStackTrace()`
        // captured by `Throwable.<init>` via `nativeFillInStackTrace`.
        NullPointerException e = new NullPointerException("x");
        System.out.println(e.getStackTrace().length > 0);
    }
}
