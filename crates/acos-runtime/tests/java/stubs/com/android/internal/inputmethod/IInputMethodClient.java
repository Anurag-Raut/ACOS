package com.android.internal.inputmethod;

import android.os.IBinder;

/**
 * Signature-only stub for the {@code @hide}
 * {@code com.android.internal.inputmethod.IInputMethodClient}. Only the type is
 * referenced by the test (as a null argument); the real interface comes from the
 * boot-classpath framework DEX at runtime.
 */
public interface IInputMethodClient {
    abstract class Stub extends android.os.Binder implements IInputMethodClient {
        public static IInputMethodClient asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
