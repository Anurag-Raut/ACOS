import java.lang.reflect.Field;

/**
 * Regression for the ACOS reflection field-slot bug.
 *
 * <p>{@code Field.artFieldIndex} is a <em>declaration index</em> while the
 * storage slot is {@code FieldDef.offset}: a {@code long}/{@code double}
 * occupies two slots. {@code Field.get} on a static field that follows a wide
 * field therefore used to read the wrong slot — this is the
 * {@code Settings.Global} NPE that blocked the M2 launch at
 * {@code PhoneWindow.<init>}.
 *
 * <p>This class deliberately interleaves wide and narrow fields, static and
 * instance, so a declaration-index read lands on the wrong slot. Every value
 * below is the declared value, so a correct ART-compatible implementation
 * reads exactly these back.
 */
public final class ReflectFieldsMain {
    // Static: every narrow field follows a wide field.
    static long S_LONG = 0x1122334455667788L;
    static int S_INT = 0x0BADF00D;
    static double S_DOUBLE = 3.141592653589793;
    static String S_REF = "static-ref";
    static boolean S_BOOL = true;
    static byte S_BYTE = (byte) 0x7f;
    static char S_CHAR = 'Z';
    static short S_SHORT = (short) 0x1234;
    static float S_FLOAT = 2.5f;

    // Instance: same interleaving.
    long iLong = 0x7766554433221100L;
    int iInt = 0x12345678;
    double iDouble = 2.718281828459045;
    String iRef = "instance-ref";
    boolean iBool = false;
    byte iByte = (byte) 0x80;
    char iChar = 'Q';
    short iShort = (short) 0x7654;
    float iFloat = 1.25f;

    static Field field(Class<?> c, String name) throws Exception {
        for (Field f : c.getDeclaredFields()) {
            if (f.getName().equals(name)) {
                return f;
            }
        }
        throw new NoSuchFieldException(name);
    }

    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    public static void main(String[] args) throws Exception {
        Class<?> c = ReflectFieldsMain.class;
        // ART's Class.getDeclaredFieldsUnchecked: instance fields first, then
        // static fields, each in declaration order.
        String[] order = {
            "iLong", "iInt", "iDouble", "iRef", "iBool", "iByte", "iChar", "iShort", "iFloat",
            "S_LONG", "S_INT", "S_DOUBLE", "S_REF", "S_BOOL", "S_BYTE", "S_CHAR", "S_SHORT",
            "S_FLOAT",
        };
        Field[] fs = c.getDeclaredFields();
        check(fs.length == order.length, "declared field count " + fs.length);
        // ART's `Class_getDeclaredFieldsUnchecked` returns the instance fields
        // first, then the static fields. The order *within* each group follows
        // the DEX field table, which d8 emits sorted by name; we pin the
        // grouping (instance-then-static) rather than the fragile byte order.
        java.util.Set<String> instanceNames = new java.util.HashSet<>(
                java.util.Arrays.asList(order).subList(0, 9));
        java.util.Set<String> staticNames = new java.util.HashSet<>(
                java.util.Arrays.asList(order).subList(9, 18));
        for (int i = 0; i < 9; i++) {
            check(instanceNames.contains(fs[i].getName()),
                    "getDeclaredFields[" + i + "]=" + fs[i].getName() + " expected an instance field");
        }
        for (int i = 9; i < 18; i++) {
            check(staticNames.contains(fs[i].getName()),
                    "getDeclaredFields[" + i + "]=" + fs[i].getName() + " expected a static field");
        }

        ReflectFieldsMain o = new ReflectFieldsMain();

        // Static typed reads.
        check(field(c, "S_LONG").getLong(null) == 0x1122334455667788L, "S_LONG");
        check(field(c, "S_INT").getInt(null) == 0x0BADF00D, "S_INT");
        check(field(c, "S_DOUBLE").getDouble(null) == 3.141592653589793, "S_DOUBLE");
        check("static-ref".equals(field(c, "S_REF").get(null)), "S_REF");
        check(field(c, "S_BOOL").getBoolean(null), "S_BOOL");
        check(field(c, "S_BYTE").getByte(null) == (byte) 0x7f, "S_BYTE");
        check(field(c, "S_CHAR").getChar(null) == 'Z', "S_CHAR");
        check(field(c, "S_SHORT").getShort(null) == (short) 0x1234, "S_SHORT");
        check(field(c, "S_FLOAT").getFloat(null) == 2.5f, "S_FLOAT");
        // Static boxed reads (Field.get must box primitives like ART).
        check(((Long) field(c, "S_LONG").get(null)) == 0x1122334455667788L, "S_LONG boxed");
        check(((Integer) field(c, "S_INT").get(null)) == 0x0BADF00D, "S_INT boxed");
        check(((Double) field(c, "S_DOUBLE").get(null)) == 3.141592653589793, "S_DOUBLE boxed");
        check(((Boolean) field(c, "S_BOOL").get(null)).booleanValue(), "S_BOOL boxed");
        check(((Byte) field(c, "S_BYTE").get(null)) == (byte) 0x7f, "S_BYTE boxed");
        check(((Character) field(c, "S_CHAR").get(null)) == 'Z', "S_CHAR boxed");
        check(((Short) field(c, "S_SHORT").get(null)) == (short) 0x1234, "S_SHORT boxed");
        check(((Float) field(c, "S_FLOAT").get(null)) == 2.5f, "S_FLOAT boxed");

        // Instance typed reads.
        check(field(c, "iLong").getLong(o) == 0x7766554433221100L, "iLong");
        check(field(c, "iInt").getInt(o) == 0x12345678, "iInt");
        check(field(c, "iDouble").getDouble(o) == 2.718281828459045, "iDouble");
        check("instance-ref".equals(field(c, "iRef").get(o)), "iRef");
        check(!field(c, "iBool").getBoolean(o), "iBool");
        check(field(c, "iByte").getByte(o) == (byte) 0x80, "iByte");
        check(field(c, "iChar").getChar(o) == 'Q', "iChar");
        check(field(c, "iShort").getShort(o) == (short) 0x7654, "iShort");
        check(field(c, "iFloat").getFloat(o) == 1.25f, "iFloat");
        check(((Long) field(c, "iLong").get(o)) == 0x7766554433221100L, "iLong boxed");

        // Static writes through the untyped setter.
        field(c, "S_LONG").set(null, Long.valueOf(-2L));
        check(S_LONG == -2L, "S_LONG set");
        field(c, "S_INT").set(null, Integer.valueOf(0x7fffffff));
        check(S_INT == 0x7fffffff, "S_INT set");
        field(c, "S_DOUBLE").set(null, Double.valueOf(-0.5));
        check(S_DOUBLE == -0.5, "S_DOUBLE set");
        field(c, "S_REF").set(null, "changed");
        check("changed".equals(S_REF), "S_REF set");
        field(c, "S_BYTE").set(null, Byte.valueOf((byte) -1));
        check(S_BYTE == (byte) -1, "S_BYTE set");

        // Instance writes through the untyped setter.
        field(c, "iLong").set(o, Long.valueOf(Long.MIN_VALUE));
        check(o.iLong == Long.MIN_VALUE, "iLong set");
        field(c, "iInt").set(o, Integer.valueOf(-12345));
        check(o.iInt == -12345, "iInt set");
        field(c, "iDouble").set(o, Double.valueOf(1234.5));
        check(o.iDouble == 1234.5, "iDouble set");
        field(c, "iRef").set(o, "changed-instance");
        check("changed-instance".equals(o.iRef), "iRef set");

        System.out.println("reflect-fields-ok");
    }
}
