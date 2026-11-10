public class LoopForever {
    public static void main(String[] args) {
        // Deliberately never terminates: the session pool must stop this via the
        // per-session HostGuard wall clock (Vm::run returns TIMEOUT) and replace
        // the session without disturbing any other session.
        long x = 0;
        while (true) {
            x++;
            if (x < 0) {
                System.out.println("unreachable " + x);
            }
        }
    }
}
