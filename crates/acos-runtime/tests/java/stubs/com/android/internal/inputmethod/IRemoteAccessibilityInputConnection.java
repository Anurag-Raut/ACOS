package com.android.internal.inputmethod;

import android.os.IBinder;

/**
 * Signature-only stub for the {@code @hide}
 * {@code com.android.internal.inputmethod.IRemoteAccessibilityInputConnection}.
 * The test passes {@code null}; the real interface comes from the framework DEX.
 */
public interface IRemoteAccessibilityInputConnection {
    abstract class Stub extends android.os.Binder implements IRemoteAccessibilityInputConnection {
        public static IRemoteAccessibilityInputConnection asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
