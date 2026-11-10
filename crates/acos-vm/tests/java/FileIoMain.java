import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.RandomAccessFile;

import android.system.ErrnoException;
import android.system.Os;
import android.system.OsConstants;

/**
 * Java-level file-I/O acceptance for the ACOS VFS (W17-B).
 *
 * Every operation runs on the real Android 14 core library
 * (`FileInputStream`, `RandomAccessFile`, `File`, `Os`); there is no Rust-side
 * shortcut. The harness parses the `key=value` lines this writes.
 *
 * Byte payloads are literals rather than `String.getBytes(...)`: the string
 * charset natives are a separate core-library workstream and are not what this
 * test measures.
 */
public class FileIoMain {
    public static void main(String[] args) throws Exception {
        // 1. FileInputStream on the boot font config.
        FileInputStream in = new FileInputStream("/system/etc/fonts.xml");
        byte[] head = new byte[20];
        int n = in.read(head);
        System.out.println("fonts_head=" + new String(head, 0, n, "UTF-8"));
        in.close();

        // 2. RandomAccessFile seek / length / getFilePointer.
        RandomAccessFile raf = new RandomAccessFile("/system/etc/fonts.xml", "r");
        raf.seek(6);
        byte[] tag = new byte[7];
        raf.readFully(tag);
        System.out.println("raf_tag=" + new String(tag, "UTF-8"));
        System.out.println("raf_pos=" + raf.getFilePointer());
        System.out.println("raf_len=" + raf.length());
        raf.seek(0);
        byte[] first = new byte[5];
        raf.readFully(first);
        System.out.println("raf_first=" + new String(first, "UTF-8"));
        raf.close();

        // 3. Write then read back in the app files dir.
        String base = "/data/user/0/com.example.app/files";
        File dir = new File(base);
        boolean made = dir.mkdirs() || dir.isDirectory();
        System.out.println("rw_dir=" + made);
        byte[] payload = { 'h', 'e', 'l', 'l', 'o', ' ', 'a', 'c', 'o', 's' };
        File probe = new File(dir, "probe.bin");
        FileOutputStream out = new FileOutputStream(probe);
        out.write(payload);
        out.close();
        FileInputStream back = new FileInputStream(probe);
        byte[] buf = new byte[payload.length];
        int got = back.read(buf);
        back.close();
        System.out.println("rw_same=" + bytesEqual(buf, got, payload));
        System.out.println("rw_len=" + probe.length());

        // The same file through the `/data/data` alias.
        FileInputStream viaDataData = new FileInputStream(
                "/data/data/com.example.app/files/probe.bin");
        byte[] buf2 = new byte[payload.length];
        int got2 = viaDataData.read(buf2);
        viaDataData.close();
        System.out.println("rw_datadata_same=" + bytesEqual(buf2, got2, payload));

        // 4. ErrnoException messages, exactly as the real class renders them.
        statTag("enoent", "/system/nope");
        openTag("erofs", "/system/etc/fonts.xml", OsConstants.O_WRONLY);
        statTag("traversal", "/system/../../etc/passwd");
        statTag("unmapped", "/etc/passwd");

        // 5. Symlink escape: planted inside the sandbox, pointing at a host
        //    path. Following it must fail closed; readlink still reports it.
        String evil = base + "/evil";
        Os.symlink("/etc/passwd", evil);
        System.out.println("readlink=" + Os.readlink(evil));
        statTag("symlink_escape", evil);
        Os.remove(evil);
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

    private static void statTag(String tag, String path) {
        try {
            Os.stat(path);
            System.out.println(tag + "_ok=true");
        } catch (ErrnoException e) {
            System.out.println(tag + "_errno=" + e.errno);
            System.out.println(tag + "_msg=" + e.getMessage());
        }
    }

    private static void openTag(String tag, String path, int flags) {
        try {
            Os.open(path, flags, 0);
            System.out.println(tag + "_ok=true");
        } catch (ErrnoException e) {
            System.out.println(tag + "_errno=" + e.errno);
            System.out.println(tag + "_msg=" + e.getMessage());
        }
    }
}
