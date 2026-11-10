public class Main {
    static class Fatal extends RuntimeException {
        Fatal(String msg) {
            super(msg);
        }
    }

    public static void main(String[] args) {
        System.out.println("about to fail");
        System.out.println("throwing Fatal now");
        if (args.length > 0) {
            throw new Fatal("fatal with arg " + args[0]);
        }
        throw new Fatal("fatal without args");
    }
}
