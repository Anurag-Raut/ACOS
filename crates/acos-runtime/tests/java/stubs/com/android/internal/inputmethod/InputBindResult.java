package com.android.internal.inputmethod;

/**
 * Signature-only stub for the {@code @hide}
 * {@code com.android.internal.inputmethod.InputBindResult}. The real class,
 * whose own {@code writeToParcel} emits the bytes
 * {@code acos-runtime::ime_java} materialises, comes from the boot-classpath
 * framework DEX at runtime.
 */
public final class InputBindResult {
    /** {@code InputBindResult.NO_IME} (a static object in API 34). */
    public static final InputBindResult NO_IME = null;

    public final int result;
    public final int sequence;

    private InputBindResult() {
        this.result = 0;
        this.sequence = 0;
    }
}
