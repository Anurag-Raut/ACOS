// Test driver for the ACOS JNI integration test.
//
// It has the same package/class name and the same `native` declarations as
// `fixtures/apps/hello-jni`, so the *real* `libhellojni.so` from the fixture APK
// resolves its `Java_dev_acos_fixtures_hellojni_MainActivity_*` exports against
// it. It deliberately does not extend `android.app.Activity`: the JNI path is
// exercised without pulling in the framework UI classes.

package dev.acos.fixtures.hellojni;

public final class MainActivity {
    private static int callbackResult = -1;
    private static String voidResult = "";

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
        callbackResult = x * 2;
        return callbackResult;
    }

    public void voidCallback(String s) {
        voidResult = s;
    }

    public static void main(String[] args) {
        MainActivity m = new MainActivity();
        System.out.println("stringFromJNI=" + m.stringFromJNI());
        System.out.println("add(20,22)=" + m.add(20, 22));
        System.out.println("sumArray([1,2,3,4,5])=" + m.sumArray(new int[] {1, 2, 3, 4, 5}));
        m.callBack();
        System.out.println("javaCallbackResult=" + callbackResult);
        System.out.println("voidCallback=" + voidResult);
        System.out.println("registeredEcho=" + m.registeredEcho("echo!"));
        try {
            m.throwFromJNI();
            System.out.println("throwFromJNI=no-exception");
        } catch (IllegalStateException e) {
            System.out.println("throwFromJNI caught=" + e.getMessage());
        }
    }
}
