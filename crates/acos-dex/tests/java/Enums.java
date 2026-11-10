public enum Enums {
    ALPHA(1) {
        @Override
        public int weight() {
            return 10;
        }
    },
    BETA(2) {
        @Override
        public int weight() {
            return 20;
        }
    },
    GAMMA(3);

    private final int code;

    Enums(int code) {
        this.code = code;
    }

    public int code() {
        return code;
    }

    public int weight() {
        return code;
    }

    public static int total() {
        int s = 0;
        for (Enums e : values()) {
            s += e.code() + e.weight();
            switch (e) {
                case ALPHA: s += 1; break;
                case BETA: s += 2; break;
                default: s += 3; break;
            }
        }
        return s;
    }

    public static void main(String[] args) {
        System.out.println(total() + Enums.valueOf("BETA").ordinal());
    }
}
