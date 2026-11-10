package com.android.internal.view;

import android.os.IBinder;
import android.os.RemoteException;
import android.view.inputmethod.EditorInfo;
import com.android.internal.inputmethod.IInputMethodClient;
import com.android.internal.inputmethod.IRemoteInputConnection;
import com.android.internal.inputmethod.IRemoteAccessibilityInputConnection;
import com.android.internal.inputmethod.InputBindResult;

/**
 * Signature-only stub for the {@code @hide}
 * {@code com.android.internal.view.IInputMethodManager}. The real interface +
 * {@code $Stub} come from the boot-classpath framework DEX at runtime; this stub
 * exists only so {@code ImeClipboardMain} compiles.
 */
public interface IInputMethodManager {
    void addClient(IInputMethodClient client, IRemoteInputConnection remote, int userId)
            throws RemoteException;

    java.util.List getInputMethodList(int userId, int directBootAware) throws RemoteException;

    java.util.List getEnabledInputMethodList(int userId) throws RemoteException;

    android.view.inputmethod.InputMethodSubtype getCurrentInputMethodSubtype(int userId)
            throws RemoteException;

    InputBindResult startInputOrWindowGainedFocus(int startInputReason,
            IInputMethodClient client, IBinder windowToken, int controlFlags,
            int softInputMode, int windowFlags, EditorInfo attribute,
            IRemoteInputConnection remoteInputConnection,
            IRemoteAccessibilityInputConnection remoteAccessibilityInputConnection,
            int unverifiedTargetSdkVersion, int userId,
            android.window.ImeOnBackInvokedDispatcher imeDispatcher) throws RemoteException;

    abstract class Stub extends android.os.Binder implements IInputMethodManager {
        public static IInputMethodManager asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
