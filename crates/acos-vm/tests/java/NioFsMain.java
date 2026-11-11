import java.io.IOException;
import java.nio.file.DirectoryStream;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.NoSuchFileException;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardCopyOption;
import java.nio.file.StandardOpenOption;
import java.nio.file.attribute.PosixFileAttributes;

/**
 * Java-level acceptance for the `sun.nio.fs` natives (W168-A).
 *
 * Exercises the real `java.nio.file.Files`/`Paths`/`FileSystem` stack on the
 * real Android 14 core library over the sandboxed VFS:
 *
 * * `Files.exists` / `notExists` / `isDirectory` / `size` (`stat0`/`stat1`/
 *   `fstat`);
 * * `Files.createDirectories` (`mkdir0`) and `Files.write`/`readAllBytes`
 *   (`open0`/`write`/`read`/`close`);
 * * `Files.newDirectoryStream` (`opendir0`/`readdir`/`closedir`);
 * * `Path.toRealPath` (`realpath0`), `Files.delete` (`unlink0`),
 *   `Files.move` (`rename0`);
 * * `Files.readAttributes` (`fstatat0` + owner via `getpwuid`);
 * * the `NoSuchFileException` reason string (`strerror`).
 */
public class NioFsMain {
    public static void main(String[] args) throws Exception {
        String base = "/data/user/0/com.example.nio/files";
        Path dir = Paths.get(base);

        // 1. createDirectories + exists + isDirectory.
        Files.createDirectories(dir);
        System.out.println("dir_exists=" + Files.exists(dir));
        System.out.println("dir_is_dir=" + Files.isDirectory(dir));

        // 2. write + readAllBytes + size.
        Path file = dir.resolve("probe.bin");
        byte[] payload = { 'h', 'e', 'l', 'l', 'o', ' ', 'a', 'c', 'o', 's' };
        Files.write(file, payload);
        byte[] back = Files.readAllBytes(file);
        System.out.println("rw_same=" + bytesEqual(back, payload));
        System.out.println("rw_size=" + Files.size(file));

        // 3. newDirectoryStream sees the file (and skips . / ..).
        java.util.List<String> names = new java.util.ArrayList<>();
        try (DirectoryStream<Path> stream = Files.newDirectoryStream(dir)) {
            for (Path p : stream) {
                names.add(p.getFileName().toString());
            }
        }
        System.out.println("list_has_probe=" + names.contains("probe.bin"));
        System.out.println("list_size=" + names.size());

        // 4. toRealPath resolves through the default filesystem.
        Path real = file.toRealPath();
        System.out.println("realpath=" + real.toString());
        System.out.println("realpath_abs=" + real.isAbsolute());

        // 5. readAttributes (stat) + owner (getpwuid).
        PosixFileAttributes attrs = Files.readAttributes(
                file, PosixFileAttributes.class, LinkOption.NOFOLLOW_LINKS);
        System.out.println("attr_is_reg=" + attrs.isRegularFile());
        System.out.println("attr_size=" + attrs.size());
        System.out.println("attr_uid=" + Files.getAttribute(file, "unix:uid"));
        System.out.println("attr_owner=" + attrs.owner().getName());

        // 6. copy-rename + delete.
        Path moved = dir.resolve("renamed.bin");
        Files.move(file, moved, StandardCopyOption.REPLACE_EXISTING);
        System.out.println("old_gone=" + !Files.exists(file));
        System.out.println("new_present=" + Files.exists(moved));
        Files.delete(moved);
        System.out.println("deleted=" + !Files.exists(moved));

        // 7. Boot-classpath file through the NIO stack.
        byte[] fonts = Files.readAllBytes(Paths.get("/system/etc/fonts.xml"));
        System.out.println("fonts_prefix=" + new String(fonts, 0, 14, "UTF-8"));

        // 8. Missing file: the real NoSuchFileException carries the strerror
        //    reason for errno 2.
        try {
            Files.readAllBytes(dir.resolve("missing.bin"));
            System.out.println("enoent_ok=true");
        } catch (NoSuchFileException e) {
            System.out.println("enoent_reason=" + e.getReason());
            System.out.println("enoent_file_present=" + (e.getFile() != null));
        }

        // 9. Reading a directory as a file must fail (EISDIR), not panic.
        try {
            Files.readAllBytes(dir);
            System.out.println("eisdir_ok=true");
        } catch (IOException e) {
            System.out.println("eisdir_caught=" + (e.getClass().getSimpleName().length() > 0));
        }
    }

    private static boolean bytesEqual(byte[] a, byte[] b) {
        if (a.length != b.length) {
            return false;
        }
        for (int i = 0; i < a.length; i++) {
            if (a[i] != b[i]) {
                return false;
            }
        }
        return true;
    }
}
