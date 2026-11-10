public class InnerClasses {
    public static class StaticNested {
        private final int v;

        public StaticNested(int v) {
            this.v = v;
        }

        public int value() {
            return v;
        }
    }

    public class Inner {
        private final int v;

        public Inner(int v) {
            this.v = v;
        }

        public int value() {
            return v + base;
        }
    }

    private final int base;

    public InnerClasses(int base) {
        this.base = base;
    }

    public int run() {
        StaticNested sn = new StaticNested(3);
        Inner in = new Inner(4);
        Runnable anon = new Runnable() {
            @Override
            public void run() {
                System.out.println("anon " + base);
            }
        };
        anon.run();
        return sn.value() + in.value();
    }

    public static void main(String[] args) {
        InnerClasses ic = new InnerClasses(10);
        System.out.println(ic.run());
    }
}
