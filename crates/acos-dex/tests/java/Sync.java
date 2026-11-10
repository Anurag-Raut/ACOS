public class Sync {
    private final Object lock = new Object();
    private int counter;

    public synchronized void inc() {
        counter++;
    }

    public synchronized int get() {
        return counter;
    }

    public void blockInc() {
        synchronized (lock) {
            counter += 2;
        }
    }

    public int nestedBlock() {
        synchronized (lock) {
            synchronized (this) {
                counter += 3;
                return counter;
            }
        }
    }

    public static synchronized int staticSync(int x) {
        return x * 2;
    }

    public static void main(String[] args) {
        Sync s = new Sync();
        s.inc();
        s.blockInc();
        System.out.println(s.get() + s.nestedBlock() + staticSync(5));
    }
}
