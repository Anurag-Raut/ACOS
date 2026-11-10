package com.android.internal.policy;

import android.content.Context;
import android.view.LayoutInflater;

/**
 * Compile-time stub of the hidden `com.android.internal.policy.PhoneLayoutInflater`.
 * Only a `--lib` for `javac`/`d8`; the VM loads the real framework class.
 */
public class PhoneLayoutInflater extends LayoutInflater {
    public PhoneLayoutInflater(Context context) {
        super(context);
    }

    @Override
    public LayoutInflater cloneInContext(Context newContext) {
        return new PhoneLayoutInflater(newContext);
    }
}
