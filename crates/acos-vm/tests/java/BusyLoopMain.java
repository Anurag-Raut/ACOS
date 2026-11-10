/**
 * A non-allocating infinite loop. Used to prove the host wall-clock watchdog
 * (`ACOS_WALL_MAX_SECS` / `HostGuard`) aborts a stuck interpreter even when it
 * never allocates and therefore never trips the guest heap limit or the host
 * RSS ceiling.
 */
public class BusyLoopMain {
    static int x;

    public static void main(String[] args) {
        while (true) {
            x++;
        }
    }
}
