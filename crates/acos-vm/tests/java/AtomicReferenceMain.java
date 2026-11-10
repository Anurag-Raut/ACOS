import java.util.concurrent.atomic.AtomicReference;

/**
 * W123-A: exercise the `AtomicReference` VarHandle surface on the real Android
 * 14 core library. Every accessor is a signature-polymorphic
 * `invoke-polymorphic` call in the DEX; the VM replaces them with a direct
 * `value` storage shim (natives/vmrt.rs), so this runs the whole surface the
 * React Native bridge uses.
 */
public class AtomicReferenceMain {
    public static void main(String[] args) {
        AtomicReference<String> r = new AtomicReference<>("a");
        System.out.println("get=" + r.get());
        System.out.println("getAndSet=" + r.getAndSet("b"));
        System.out.println("after=" + r.get());
        System.out.println("cas-ok=" + r.compareAndSet("b", "c"));
        System.out.println("cas-fail=" + r.compareAndSet("x", "d"));
        System.out.println("val=" + r.get());
        System.out.println("weak=" + r.weakCompareAndSet("c", "e"));
        System.out.println("final=" + r.get());
        AtomicReference<String> n = new AtomicReference<>();
        System.out.println("null=" + (n.get() == null));
        System.out.println("cas-null=" + n.compareAndSet(null, "z"));
        System.out.println("n=" + n.get());
    }
}
