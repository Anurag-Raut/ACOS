package dev.acos.fixtures.hellojni;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.widget.TextView;

/**
 * JNI fixture activity. Exercises, through libhellojni.so:
 *  - a plain native String return
 *  - primitive argument/return
 *  - primitive array argument
 *  - native -> Java callbacks (static int and instance void)
 *  - a native method registered via RegisterNatives in JNI_OnLoad
 *  - a native method that throws a Java exception (ThrowNew)
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosJniFixture";

    private static int callbackResult = -1;
    private String voidResult = "";

    static {
        System.loadLibrary("hellojni");
    }

    private native String stringFromJNI();

    private native int add(int a, int b);

    private native int sumArray(int[] values);

    private native void callBack();

    private native String registeredEcho(String s);

    private native void throwFromJNI();

    public static int javaCallback(int x) {
        Log.i(TAG, "javaCallback x=" + x);
        callbackResult = x * 2;
        return callbackResult;
    }

    public void voidCallback(String s) {
        Log.i(TAG, "voidCallback s=" + s);
        voidResult = s;
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        StringBuilder sb = new StringBuilder();
        sb.append("stringFromJNI=").append(stringFromJNI()).append('\n');
        sb.append("add(20,22)=").append(add(20, 22)).append('\n');
        sb.append("sumArray([1,2,3,4,5])=").append(sumArray(new int[]{1, 2, 3, 4, 5})).append('\n');
        callBack();
        sb.append("javaCallbackResult=").append(callbackResult).append('\n');
        sb.append("voidCallback=").append(voidResult).append('\n');
        sb.append("registeredEcho=").append(registeredEcho("echo!")).append('\n');
        try {
            throwFromJNI();
            sb.append("throwFromJNI=no-exception\n");
        } catch (IllegalStateException e) {
            sb.append("throwFromJNI caught=").append(e.getMessage()).append('\n');
        }
        TextView tv = new TextView(this);
        tv.setText(sb.toString());
        setContentView(tv);
    }
}
