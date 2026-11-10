import java.io.File;
import java.io.FileDescriptor;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.lang.reflect.Field;
import java.lang.reflect.Modifier;
import java.util.ArrayList;
import java.util.List;

import android.system.ErrnoException;
import android.system.Os;
import android.system.OsConstants;
import android.system.StructStat;

/**
 * Java-level acceptance for `android.system.OsConstants` (W25-B).
 *
 * `OsConstants.<clinit>` assigns every `public static final int` from
 * `placeholder()` (all zero) and then calls the `initConstants` native. This
 * program runs on the real Android 14 core library and:
 *
 *   1. prints the constants the framework's file/socket/errno code branches
 *      on (a zero anywhere changes program behaviour);
 *   2. folds the full 538-field static table into an FNV-1a 64 checksum so the
 *      Rust harness can pin every value against the ART capture without
 *      hard-coding 538 pairs twice;
 *   3. exercises the constants through real `Os`/`java.io` round trips: a
 *      file write/read-back, `Os.stat`/`Os.lstat`, and an `Os.pipe2` pipe.
 *
 * `errnoName`/`gaiName` are pure-Java and read the table, so their output is
 * itself a check that the `<clinit>` ran.
 */
public class OsConstantsMain {
    public static void main(String[] args) throws Exception {
        // 1. The constants the framework actually switches on.
        System.out.println("O_RDONLY=" + OsConstants.O_RDONLY);
        System.out.println("O_WRONLY=" + OsConstants.O_WRONLY);
        System.out.println("O_RDWR=" + OsConstants.O_RDWR);
        System.out.println("O_CREAT=" + OsConstants.O_CREAT);
        System.out.println("O_APPEND=" + OsConstants.O_APPEND);
        System.out.println("O_CLOEXEC=" + OsConstants.O_CLOEXEC);
        System.out.println("EACCES=" + OsConstants.EACCES);
        System.out.println("ENOENT=" + OsConstants.ENOENT);
        System.out.println("EEXIST=" + OsConstants.EEXIST);
        System.out.println("S_IFMT=" + OsConstants.S_IFMT);
        System.out.println("S_IFDIR=" + OsConstants.S_IFDIR);
        System.out.println("S_IFREG=" + OsConstants.S_IFREG);
        System.out.println("AF_INET=" + OsConstants.AF_INET);
        System.out.println("AF_INET6=" + OsConstants.AF_INET6);
        System.out.println("SOCK_STREAM=" + OsConstants.SOCK_STREAM);
        System.out.println("SOCK_DGRAM=" + OsConstants.SOCK_DGRAM);
        System.out.println("IPPROTO_TCP=" + OsConstants.IPPROTO_TCP);
        System.out.println("PROT_READ=" + OsConstants.PROT_READ);
        System.out.println("PROT_WRITE=" + OsConstants.PROT_WRITE);
        System.out.println("POLLIN=" + OsConstants.POLLIN);
        System.out.println("POLLOUT=" + OsConstants.POLLOUT);
        System.out.println("SIGKILL=" + OsConstants.SIGKILL);
        System.out.println("SIGTERM=" + OsConstants.SIGTERM);
        System.out.println("SIGRTMIN=" + OsConstants.SIGRTMIN);
        System.out.println("SIGRTMAX=" + OsConstants.SIGRTMAX);
        System.out.println("MAP_PRIVATE=" + OsConstants.MAP_PRIVATE);
        System.out.println("SEEK_SET=" + OsConstants.SEEK_SET);
        System.out.println("SEEK_END=" + OsConstants.SEEK_END);
        System.out.println("_SC_PAGESIZE=" + OsConstants._SC_PAGESIZE);
        System.out.println("_SC_PAGE_SIZE=" + OsConstants._SC_PAGE_SIZE);
        System.out.println("_SC_CLK_TCK=" + OsConstants._SC_CLK_TCK);
        System.out.println("_SC_OPEN_MAX=" + OsConstants._SC_OPEN_MAX);
        System.out.println("AI_ADDRCONFIG=" + OsConstants.AI_ADDRCONFIG);
        System.out.println("EAI_NONAME=" + OsConstants.EAI_NONAME);
        System.out.println("NI_NUMERICHOST=" + OsConstants.NI_NUMERICHOST);

        // Pure-Java consumers of the table.
        System.out.println("errnoName_13=" + OsConstants.errnoName(OsConstants.EACCES));
        System.out.println("errnoName_2=" + OsConstants.errnoName(OsConstants.ENOENT));
        System.out.println("gaiName_8=" + OsConstants.gaiName(OsConstants.EAI_NONAME));
        System.out.println("S_ISDIR_16384=" + OsConstants.S_ISDIR(OsConstants.S_IFDIR));
        System.out.println("S_ISDIR_32768=" + OsConstants.S_ISDIR(OsConstants.S_IFREG));

        // 2. Full-table checksum + count.
        System.out.println("field_count=" + staticIntCount());
        System.out.println("hash=" + Long.toUnsignedString(tableHash()));

        // 3. `Os`/`java.io` round trips driven by the same constants.
        // The ART oracle runs as the shell uid and passes a writable
        // `/data/local/tmp` base; the ACOS harness uses the sandbox files dir.
        String base = args.length > 0
                ? args[0]
                : "/data/user/0/com.example.osconst/files";
        File dir = new File(base);
        if (!dir.isDirectory() && !dir.mkdirs()) {
            throw new IllegalStateException("cannot create sandbox dir " + base);
        }
        byte[] payload = { 'o', 's', 'c', 'o', 'n', 's', 't' };
        File probe = new File(dir, "roundtrip.bin");
        FileOutputStream out = new FileOutputStream(probe);
        out.write(payload);
        out.close();
        FileInputStream in = new FileInputStream(probe);
        byte[] back = new byte[payload.length];
        int got = in.read(back);
        in.close();
        System.out.println("file_same=" + bytesEqual(back, got, payload));
        System.out.println("file_len=" + probe.length());

        StructStat st = Os.stat(probe.getAbsolutePath());
        System.out.println("stat_type_dir=" + OsConstants.S_ISDIR(st.st_mode));
        System.out.println("stat_type_reg=" + OsConstants.S_ISREG(st.st_mode));
        System.out.println("stat_size=" + st.st_size);
        StructStat ls = Os.lstat(probe.getAbsolutePath());
        System.out.println("lstat_is_reg=" + OsConstants.S_ISREG(ls.st_mode));

        // `access` is the check the framework uses before opening. On a
        // missing path the real `libcore.io.Linux` native raises
        // `ErrnoException(ENOENT)`, so the errno round-trip is asserted.
        System.out.println("access_f_ok=" + Os.access(probe.getAbsolutePath(), OsConstants.F_OK));
        System.out.println("access_r_ok=" + Os.access(probe.getAbsolutePath(), OsConstants.R_OK));
        try {
            boolean missing = Os.access(base + "/nope", OsConstants.F_OK);
            System.out.println("access_missing=" + missing);
        } catch (ErrnoException e) {
            System.out.println("access_missing_errno=" + e.errno);
        }

        // An `Os.pipe2` pipe, written and read through `Os`.
        FileDescriptor[] pipe = Os.pipe();
        byte[] ping = { 9, 8, 7 };
        int written = Os.write(pipe[1], ping, 0, ping.length);
        byte[] pong = new byte[3];
        int n = Os.read(pipe[0], pong, 0, pong.length);
        System.out.println("pipe_written=" + written);
        System.out.println("pipe_read=" + n);
        System.out.println("pipe_same=" + bytesEqual(pong, n, ping));
        Os.close(pipe[0]);
        Os.close(pipe[1]);

        // 4. Newly implemented sandbox-safe natives.
        System.out.println("umask=" + Os.umask(OsConstants.S_IRWXU));
        System.out.println("sysconf_pagesize=" + Os.sysconf(OsConstants._SC_PAGESIZE));
        System.out.println("sysconf_clk_tck=" + Os.sysconf(OsConstants._SC_CLK_TCK));
        System.out.println("strsignal_9=" + Os.strsignal(OsConstants.SIGKILL));
        System.out.println("gai_strerror_8=" + Os.gai_strerror(OsConstants.EAI_NONAME));
        System.out.println("environ_empty=" + (Os.environ().length == 0));

        // readv/writev across a pipe.
        FileDescriptor[] vpipe = Os.pipe();
        byte[] a = { 1, 2 };
        byte[] b = { 3, 4, 5 };
        int vw = Os.writev(vpipe[1], new Object[] { a, b }, new int[] { 0, 0 },
                new int[] { a.length, b.length });
        byte[] ra = new byte[2];
        byte[] rb = new byte[3];
        int vr = Os.readv(vpipe[0], new Object[] { ra, rb }, new int[] { 0, 0 },
                new int[] { ra.length, rb.length });
        System.out.println("writev=" + vw);
        System.out.println("readv=" + vr);
        System.out.println("v_same=" + (ra[0] == 1 && ra[1] == 2 && rb[0] == 3 && rb[1] == 4 && rb[2] == 5));
        Os.close(vpipe[0]);
        Os.close(vpipe[1]);

        // AF_UNIX socketpair, written/read through `Os`.
        FileDescriptor sp0 = new FileDescriptor();
        FileDescriptor sp1 = new FileDescriptor();
        Os.socketpair(OsConstants.AF_UNIX, OsConstants.SOCK_STREAM, 0, sp0, sp1);
        byte[] sping = { 11, 22 };
        Os.write(sp1, sping, 0, sping.length);
        byte[] spong = new byte[2];
        int spr = Os.read(sp0, spong, 0, spong.length);
        System.out.println("socketpair_read=" + spr);
        System.out.println("socketpair_same=" + (spong[0] == 11 && spong[1] == 22));
        Os.close(sp0);
        Os.close(sp1);

        // Anonymous memory fd.
        FileDescriptor mfd = Os.memfd_create("acos_test", OsConstants.MFD_CLOEXEC);
        byte[] mpay = { 7, 7, 7 };
        Os.write(mfd, mpay, 0, mpay.length);
        Os.lseek(mfd, 0, OsConstants.SEEK_SET);
        byte[] mback = new byte[3];
        int mn = Os.read(mfd, mback, 0, mback.length);
        System.out.println("memfd_read=" + mn);
        System.out.println("memfd_same=" + (mback[0] == 7 && mback[1] == 7 && mback[2] == 7));
        Os.close(mfd);

        // posix_fallocate grows the file to offset + len.
        FileDescriptor pfd = Os.open(probe.getAbsolutePath(), OsConstants.O_RDWR, 0);
        Os.posix_fallocate(pfd, 0, 16);
        System.out.println("fallocate_size=" + Os.fstat(pfd).st_size);
        Os.close(pfd);

        System.out.println("done");
    }

    /** Number of `static` `int` fields declared by `OsConstants`. */
    private static int staticIntCount() {
        int n = 0;
        for (Field f : OsConstants.class.getDeclaredFields()) {
            if (Modifier.isStatic(f.getModifiers()) && f.getType() == int.class) {
                n++;
            }
        }
        return n;
    }

    /**
     * FNV-1a 64 over `name=value\n` for every static `int` field, sorted by
     * name. Deterministic and trivial to reproduce in Rust.
     */
    private static long tableHash() throws Exception {
        List<String> names = new ArrayList<String>();
        for (Field f : OsConstants.class.getDeclaredFields()) {
            if (Modifier.isStatic(f.getModifiers()) && f.getType() == int.class) {
                names.add(f.getName());
            }
        }
        insertionSort(names);
        long h = 0xcbf29ce484222325L;
        for (String name : names) {
            Field f = OsConstants.class.getDeclaredField(name);
            String line = name + "=" + f.getInt(null) + "\n";
            for (int i = 0; i < line.length(); i++) {
                h ^= (long) (line.charAt(i) & 0xff);
                h *= 0x100000001b3L;
            }
        }
        return h;
    }

    private static void insertionSort(List<String> a) {
        for (int i = 1; i < a.size(); i++) {
            String key = a.get(i);
            int j = i - 1;
            while (j >= 0 && a.get(j).compareTo(key) > 0) {
                a.set(j + 1, a.get(j));
                j--;
            }
            a.set(j + 1, key);
        }
    }

    private static boolean bytesEqual(byte[] a, int n, byte[] b) {
        if (n != b.length) {
            return false;
        }
        for (int i = 0; i < n; i++) {
            if (a[i] != b[i]) {
                return false;
            }
        }
        return true;
    }
}
