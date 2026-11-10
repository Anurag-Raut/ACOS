package dev.acos.fixtures.broker;

/** Java side of the brokered native-thread JNI tests (W57-A). */
public class BrokerTarget {
    public static int base = 100;

    public static int add(int a, int b) {
        return a + b;
    }

    public static String echo(String s) {
        return s;
    }

    public int twice(int x) {
        return x * 2;
    }

    public static void boom() {
        throw new IllegalStateException("broker-boom");
    }

    public static int readBase() {
        return base;
    }

    public static void setBase(int v) {
        base = v;
    }
}
