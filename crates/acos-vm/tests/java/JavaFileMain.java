import java.io.File;
import java.io.FileOutputStream;
import java.util.Arrays;

/**
 * Java-level acceptance for the `java.io.UnixFileSystem` natives (W181-A).
 *
 * Exercises the real `java.io.File` API on the real Android 14 core library
 * over the sandboxed VFS: `createDirectory` + `getBooleanAttributes0`,
 * `createFileExclusively`, `getLength`, `checkAccess`, `setPermission`
 * (`setReadable`/`setWritable`/`setExecutable`), `setReadOnly`,
 * `getLastModifiedTime`/`setLastModifiedTime`, `rename0`, `delete0` and
 * `list`.
 *
 * The same class is compiled with the host `javac` and run on the host JVM
 * (real OpenJDK `java.io.UnixFileSystem`) and on ACOS, so the printed
 * `key=value` lines are a direct comparison with real Java. Everything printed
 * here is deliberately path- and environment-independent (booleans, lengths,
 * sorted names) so both runs must agree verbatim.
 */
public class JavaFileMain {
    public static void main(String[] args) throws Exception {
        File base = new File(args.length > 0 ? args[0] : "/data/user/0/com.example.file/files");
        File dir = new File(base, "ufs-probe");
        cleanup(dir);
        base.mkdirs();

        // 1. createDirectory + attribute bits.
        System.out.println("mkdir=" + dir.mkdir());
        System.out.println("mkdir_again=" + dir.mkdir());
        System.out.println("dir_exists=" + dir.exists());
        System.out.println("dir_is_dir=" + dir.isDirectory());
        System.out.println("dir_is_file=" + dir.isFile());
        System.out.println("dir_hidden=" + dir.isHidden());
        System.out.println("dir_canonical_suffix="
                + dir.getCanonicalPath().endsWith("/ufs-probe"));
        System.out.println("dir_can_read=" + dir.canRead());

        // 2. createFileExclusively + getLength/getBooleanAttributes0.
        File file = new File(dir, "a.bin");
        System.out.println("create_new=" + file.createNewFile());
        System.out.println("create_again=" + file.createNewFile());
        System.out.println("file_exists=" + file.exists());
        System.out.println("file_is_file=" + file.isFile());
        System.out.println("file_length_empty=" + file.length());
        byte[] payload = new byte[7];
        Arrays.fill(payload, (byte) 'A');
        try (FileOutputStream out = new FileOutputStream(file)) {
            out.write(payload);
        }
        System.out.println("file_length_after=" + file.length());

        // 3. checkAccess + setPermission.
        System.out.println("can_read=" + file.canRead());
        System.out.println("can_write=" + file.canWrite());
        System.out.println("can_execute_new=" + file.canExecute());
        System.out.println("set_readonly=" + file.setReadOnly());
        System.out.println("can_write_after_readonly=" + file.canWrite());
        System.out.println("can_read_after_readonly=" + file.canRead());
        System.out.println("set_writable_owner=" + file.setWritable(true, true));
        System.out.println("can_write_restored=" + file.canWrite());
        System.out.println("set_executable_owner=" + file.setExecutable(true, true));
        System.out.println("can_execute_set=" + file.canExecute());
        System.out.println("set_executable_off=" + file.setExecutable(false, false));
        System.out.println("can_execute_cleared=" + file.canExecute());
        System.out.println("set_readable_off_other=" + file.setReadable(false, false));
        System.out.println("can_read_owner_kept=" + file.canRead());
        System.out.println("set_readable_on=" + file.setReadable(true, false));
        System.out.println("can_read_restored=" + file.canRead());

        // 4. getLastModifiedTime / setLastModifiedTime.
        long stamp = 1_600_000_000_000L;
        System.out.println("set_last_modified=" + file.setLastModified(stamp));
        System.out.println("last_modified=" + file.lastModified());

        // 5. rename0 + delete0.
        File moved = new File(dir, "b.bin");
        System.out.println("rename=" + file.renameTo(moved));
        System.out.println("old_exists=" + file.exists());
        System.out.println("new_exists=" + moved.exists());
        System.out.println("delete=" + moved.delete());
        System.out.println("delete_again=" + moved.delete());
        System.out.println("new_exists_after_delete=" + moved.exists());

        // 6. list (sorted; the probe dir holds only the hidden file now).
        File hidden = new File(dir, ".hidden");
        System.out.println("hidden_created=" + hidden.createNewFile());
        System.out.println("hidden_is_hidden=" + hidden.isHidden());
        String[] names = dir.list();
        Arrays.sort(names);
        System.out.println("list=" + String.join(",", names));
        System.out.println("list_len=" + names.length);

        // 7. getFreeSpace goes through getSpace(File,int).
        System.out.println("free_space_positive=" + (dir.getFreeSpace() > 0));

        // 8. Missing-file behaviour: length 0, attributes false, delete false,
        //    rename false (no exception from the native layer).
        File missing = new File(dir, "missing.bin");
        System.out.println("missing_exists=" + missing.exists());
        System.out.println("missing_length=" + missing.length());
        System.out.println("missing_delete=" + missing.delete());
        System.out.println("missing_rename=" + missing.renameTo(new File(dir, "x.bin")));
        System.out.println("missing_can_read=" + missing.canRead());

        hidden.delete();
        dir.delete();
    }

    private static void cleanup(File dir) {
        File[] existing = dir.listFiles();
        if (existing != null) {
            for (File f : existing) {
                f.delete();
            }
        }
        dir.delete();
    }
}
