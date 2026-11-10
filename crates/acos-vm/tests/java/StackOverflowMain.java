public class StackOverflowMain {
    static int depth = 0;
    static void rec() { depth++; rec(); }
    public static void main(String[] args) {
        try { rec(); System.out.println("no-overflow"); }
        catch (StackOverflowError e) { System.out.println("caught-stack-overflow"); }
        System.out.println("done");
    }
}
