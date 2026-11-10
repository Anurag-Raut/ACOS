import java.util.EnumSet;

public class Main {
    enum Color {
        RED("r"), GREEN("g"), BLUE("b");

        private final String code;

        Color(String code) {
            this.code = code;
        }

        String code() {
            return code;
        }
    }

    enum Op {
        ADD {
            @Override
            int apply(int a, int b) {
                return a + b;
            }
        },
        SUB {
            @Override
            int apply(int a, int b) {
                return a - b;
            }
        },
        MUL {
            @Override
            int apply(int a, int b) {
                return a * b;
            }
        };

        abstract int apply(int a, int b);
    }

    static String describe(Color c) {
        switch (c) {
            case RED: return "red!";
            case GREEN: return "green!";
            case BLUE: return "blue!";
            default: return "?";
        }
    }

    public static void main(String[] args) {
        for (Color c : Color.values()) {
            System.out.println("color=" + c + " ordinal=" + c.ordinal() + " name=" + c.name() + " code=" + c.code());
        }
        System.out.println("valueOf=" + Color.valueOf("GREEN"));
        System.out.println("RED.compareTo(BLUE)=" + Color.RED.compareTo(Color.BLUE));
        for (Color c : Color.values()) {
            System.out.println("describe=" + describe(c));
        }
        for (Op op : Op.values()) {
            System.out.println("op=" + op + " apply(6,3)=" + op.apply(6, 3));
        }
        EnumSet<Color> set = EnumSet.of(Color.RED, Color.BLUE);
        System.out.println("set=" + set + " contains=" + set.contains(Color.RED) + " " + set.contains(Color.GREEN));
        System.out.println("color.class.isEnum=" + Color.class.isEnum());
        System.out.println("RED.declaringClass=" + Color.RED.getDeclaringClass().getSimpleName());
        System.out.println("switchEnum=" + describe(Color.BLUE));
        System.out.println("values.length=" + Color.values().length);
    }
}
