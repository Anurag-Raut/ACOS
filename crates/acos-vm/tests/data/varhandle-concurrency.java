import java.lang.invoke.MethodHandles;
import java.lang.invoke.VarHandle;
import java.util.concurrent.ConcurrentLinkedQueue;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicIntegerFieldUpdater;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.atomic.AtomicReferenceFieldUpdater;

public class Main {
    static int sInt;
    static long sLong;
    static Object sRef;
    volatile int iInt;
    long iLong;
    volatile Object iRef;
    volatile int vInt;
    volatile long vLong;
    volatile Object vRef;
    int[] arr = new int[4];

    static final VarHandle S_INT;
    static final VarHandle S_LONG;
    static final VarHandle S_REF;
    static final VarHandle I_INT;
    static final VarHandle I_LONG;
    static final VarHandle I_REF;
    static final VarHandle V_INT;
    static final VarHandle V_LONG;
    static final VarHandle V_REF;
    static final VarHandle ARR;
    static {
        try {
            MethodHandles.Lookup l = MethodHandles.lookup();
            S_INT = l.findStaticVarHandle(Main.class, "sInt", int.class);
            S_LONG = l.findStaticVarHandle(Main.class, "sLong", long.class);
            S_REF = l.findStaticVarHandle(Main.class, "sRef", Object.class);
            I_INT = l.findVarHandle(Main.class, "iInt", int.class);
            I_LONG = l.findVarHandle(Main.class, "iLong", long.class);
            I_REF = l.findVarHandle(Main.class, "iRef", Object.class);
            V_INT = l.findVarHandle(Main.class, "vInt", int.class);
            V_LONG = l.findVarHandle(Main.class, "vLong", long.class);
            V_REF = l.findVarHandle(Main.class, "vRef", Object.class);
            ARR = MethodHandles.arrayElementVarHandle(int[].class);
        } catch (Throwable t) {
            throw new ExceptionInInitializerError(t);
        }
    }

    static class Holder {
        volatile long value;
        static final VarHandle VALUE;
        static {
            try {
                VALUE = MethodHandles.lookup().findVarHandle(Holder.class, "value", long.class);
            } catch (Throwable t) { throw new ExceptionInInitializerError(t); }
        }
    }

    public static void main(String[] args) throws Exception {
        S_INT.set(41);
        System.out.println("static int set/get=" + S_INT.get() + " add=" + S_INT.getAndAdd(1));
        S_LONG.set(1L << 40);
        System.out.println("static long=" + S_LONG.getAndAdd(5L) + " -> " + S_LONG.get());
        S_REF.set("hello");
        System.out.println("static ref=" + S_REF.getAndSet("world") + " -> " + S_REF.get());

        Main m = new Main();
        I_INT.set(m, 7);
        I_LONG.set(m, 1234567890123L);
        I_REF.set(m, "x");
        System.out.println("inst int=" + I_INT.get(m) + " long=" + I_LONG.get(m) + " ref=" + I_REF.get(m));
        System.out.println("cas=" + I_INT.compareAndSet(m, 7, 8) + " now=" + I_INT.get(m));
        System.out.println("cae=" + I_INT.compareAndExchange(m, 8, 9) + " now=" + I_INT.get(m));

        V_INT.set(m, 3);
        System.out.println("vol int=" + V_INT.getAndAdd(m, 4) + " -> " + V_INT.get(m));
        V_LONG.set(m, 9L);
        System.out.println("vol long=" + V_LONG.compareAndSet(m, 9L, 11L) + " -> " + V_LONG.get(m));
        V_REF.set(m, "a");
        System.out.println("vol ref=" + V_REF.compareAndSet(m, "a", "b") + " -> " + V_REF.get(m));
        System.out.println("bitwise=" + V_INT.getAndBitwiseOr(m, 0x10) + " -> " + V_INT.get(m));

        ARR.set(m.arr, 2, 42);
        System.out.println("arr=" + ARR.get(m.arr, 2) + " cas=" + ARR.compareAndSet(m.arr, 2, 42, 43) + " -> " + ARR.get(m.arr, 2));

        Holder h = new Holder();
        System.out.println("holder cas=" + Holder.VALUE.compareAndSet(h, 0L, 5L) + " -> " + Holder.VALUE.get(h));

        ConcurrentLinkedQueue<Integer> q = new ConcurrentLinkedQueue<>();
        for (int i = 0; i < 100; i++) q.add(i);
        int sum = 0;
        for (Integer v : q) sum += v;
        System.out.println("clq size=" + q.size() + " sum=" + sum + " poll=" + q.poll());

        ConcurrentHashMap<String, Integer> map = new ConcurrentHashMap<>();
        for (int i = 0; i < 50; i++) map.put("k" + i, i);
        System.out.println("chm size=" + map.size() + " get=" + map.get("k7"));

        AtomicInteger ai = new AtomicInteger(5);
        System.out.println("ai=" + ai.getAndIncrement() + " -> " + ai.get());
        AtomicLong al = new AtomicLong(100);
        System.out.println("al=" + al.getAndAdd(11) + " -> " + al.get());

        AtomicIntegerFieldUpdater<Main> u = AtomicIntegerFieldUpdater.newUpdater(Main.class, "iInt");
        System.out.println("aifu=" + u.getAndAdd(m, 5) + " -> " + m.iInt);
        AtomicReferenceFieldUpdater<Main, Object> ru = AtomicReferenceFieldUpdater.newUpdater(Main.class, Object.class, "iRef");
        System.out.println("arfu=" + ru.compareAndSet(m, "x", "y") + " -> " + m.iRef);
        System.out.println("done");
    }
}
