public class Main {
    static class A {
    }

    static class B extends A {
    }

    static class C extends B {
    }

    static void check(Object o) {
        System.out.println("null=" + (o == null)
                + " A=" + (o instanceof A)
                + " B=" + (o instanceof B)
                + " C=" + (o instanceof C)
                + " String=" + (o instanceof String)
                + " int[]=" + (o instanceof int[]));
    }

    public static void main(String[] args) {
        Object[] vals = {new A(), new B(), new C(), "str", Integer.valueOf(3), new int[2], new Object[0]};
        for (Object v : vals) {
            System.out.print("val=" + v.getClass().getSimpleName() + " ");
            check(v);
        }
        A a = new C();
        System.out.println("cast1=" + (a instanceof C));
        System.out.println("cast2=" + ((C) a).getClass().getName());
        Number n = Integer.valueOf(7);
        System.out.println("num=" + (n instanceof Integer) + " " + (n instanceof Number));
        Object o = "hello";
        System.out.println("objString=" + (o instanceof String) + " len=" + ((String) o).length());
        Object bad = Integer.valueOf(1);
        try {
            String s = (String) bad;
            System.out.println("unreachable=" + s);
        } catch (ClassCastException e) {
            System.out.println("caught=" + e.getClass().getName());
        }
        Object[] arr = new String[2];
        arr[0] = "a";
        System.out.println("arrayStore.type=" + arr.getClass().getName());
        try {
            arr[1] = Integer.valueOf(1);
            System.out.println("unreachable");
        } catch (ArrayStoreException e) {
            System.out.println("caught=" + e.getClass().getName());
        }
        System.out.println("primitive.array=" + (new int[0] instanceof int[]));
        System.out.println("arrayIsObject=" + (new int[0] instanceof Object));
        System.out.println("nullCast=" + ((A) null == null));
    }
}
