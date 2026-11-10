import java.lang.reflect.Field;

/**
 * W95-A regression for the static-reflection divergence (W88-A blocker 3).
 *
 * `arr` has a non-constant initializer, so the class `<clinit>` explicitly
 * stores `null` into it. The JLS (and ART) require `Field.set` on a static
 * field to initialise the declaring class *first*; ACOS used to write the
 * array and only run `<clinit>` on the later compiled `getstatic`, which
 * clobbered the write back to null.
 *
 * The `long wide` field is declared before the target so its two slots shift
 * `arr`'s slot beyond its declaration index (the `artFieldIndex` vs `offset`
 * divergence).
 */
public class ReflectStaticInitMain {
    static long wide = 0x1122334455667788L;
    static volatile Object[] arr = makeNull();
    static Object plain;

    static Object[] makeNull() {
        return null;
    }

    public static void main(String[] args) throws Exception {
        Field f = ReflectStaticInitMain.class.getDeclaredField("arr");
        f.setAccessible(true);
        f.set(null, new Object[] {"x"});
        // Reflection readback.
        System.out.println(f.get(null) != null);
        // Compiled `getstatic`: triggers `<clinit>` if the VM skipped it.
        System.out.println(arr != null && arr.length == 1);
        System.out.println(wide == 0x1122334455667788L);
        f.set(null, null);
        System.out.println(arr == null);
    }
}
