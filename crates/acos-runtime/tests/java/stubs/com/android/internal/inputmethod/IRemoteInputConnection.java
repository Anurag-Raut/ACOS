package com.android.internal.inputmethod;

import android.os.IBinder;

/**
 * Signature-only stub for the {@code @hide}
 * {@code com.android.internal.inputmethod.IRemoteInputConnection}. The test
 * passes {@code null}; the real interface comes from the framework DEX.
 */
public interface IRemoteInputConnection {
    abstract class Stub extends android.os.Binder implements IRemoteInputConnection {
        public static IRemoteInputConnection asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
