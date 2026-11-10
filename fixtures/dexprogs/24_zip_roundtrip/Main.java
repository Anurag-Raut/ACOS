import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.util.zip.Adler32;
import java.util.zip.CRC32;
import java.util.zip.Deflater;
import java.util.zip.GZIPInputStream;
import java.util.zip.GZIPOutputStream;
import java.util.zip.Inflater;
import java.util.zip.ZipEntry;
import java.util.zip.ZipInputStream;
import java.util.zip.ZipOutputStream;

/**
 * Deterministic java.util.zip round-trip fixture.
 *
 * Every printed value is a property of the *uncompressed* data (lengths,
 * checksums, ASCII content) or a round-trip boolean. Compressed byte counts
 * and bytes are never printed, because the zlib implementation (host JVM vs
 * ART vs ACOS/miniz_oxide) may choose different encodings.
 */
public class Main {
    private static final String PATTERN = "ACOS zip roundtrip payload ";

    static byte[] asciiBytes(String s) {
        byte[] b = new byte[s.length()];
        for (int i = 0; i < s.length(); i++) {
            b[i] = (byte) s.charAt(i);
        }
        return b;
    }

    static String ascii(byte[] b) {
        char[] c = new char[b.length];
        for (int i = 0; i < b.length; i++) {
            c[i] = (char) (b[i] & 0xff);
        }
        return new String(c);
    }

    static boolean eq(byte[] a, byte[] b) {
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

    static byte[] payload(int len) {
        byte[] p = asciiBytes(PATTERN);
        byte[] d = new byte[len];
        for (int i = 0; i < len; i++) {
            d[i] = p[i % p.length];
        }
        return d;
    }

    public static void main(String[] args) throws Exception {
        byte[] data = payload(300);
        System.out.println("payload_len=" + data.length);
        System.out.println("payload_ascii=" + ascii(data));

        CRC32 crc = new CRC32();
        crc.update(data, 0, data.length);
        System.out.println("crc32=" + crc.getValue());
        CRC32 crcByte = new CRC32();
        for (byte b : data) {
            crcByte.update(b);
        }
        System.out.println("crc32_bytewise=" + crcByte.getValue());
        System.out.println("crc32_match=" + (crc.getValue() == crcByte.getValue()));

        Adler32 adler = new Adler32();
        adler.update(data, 0, data.length);
        System.out.println("adler32=" + adler.getValue());

        int[] levels = {0, 1, 6, 9};
        for (int level : levels) {
            Deflater def = new Deflater(level);
            def.setInput(data);
            def.finish();
            ByteArrayOutputStream compressed = new ByteArrayOutputStream();
            byte[] buf = new byte[64];
            while (!def.finished()) {
                int n = def.deflate(buf);
                compressed.write(buf, 0, n);
            }
            System.out.println("deflate_level_" + level + "_adler=" + def.getAdler());
            def.end();

            Inflater inf = new Inflater();
            inf.setInput(compressed.toByteArray());
            byte[] out = new byte[data.length];
            int n = inf.inflate(out);
            System.out.println("inflate_level_" + level + "_ok="
                    + (n == data.length && eq(out, data)));
            System.out.println("inflate_level_" + level + "_adler=" + inf.getAdler());
            inf.end();
        }

        // Raw (nowrap) deflate/inflate, the encoding used inside ZIP entries.
        Deflater rawDef = new Deflater(6, true);
        rawDef.setInput(data);
        rawDef.finish();
        ByteArrayOutputStream rawCompressed = new ByteArrayOutputStream();
        byte[] rbuf = new byte[64];
        while (!rawDef.finished()) {
            int n = rawDef.deflate(rbuf);
            rawCompressed.write(rbuf, 0, n);
        }
        rawDef.end();
        Inflater rawInf = new Inflater(true);
        rawInf.setInput(rawCompressed.toByteArray());
        byte[] rawOut = new byte[data.length];
        int rn = rawInf.inflate(rawOut);
        System.out.println("raw_deflate_ok=" + (rn == data.length && eq(rawOut, data)));
        rawInf.end();

        // GZIPOutputStream / GZIPInputStream round-trip.
        ByteArrayOutputStream gz = new ByteArrayOutputStream();
        GZIPOutputStream gos = new GZIPOutputStream(gz);
        gos.write(data);
        gos.close();
        GZIPInputStream gis = new GZIPInputStream(new ByteArrayInputStream(gz.toByteArray()));
        ByteArrayOutputStream gzOut = new ByteArrayOutputStream();
        byte[] gbuf = new byte[64];
        int gn;
        while ((gn = gis.read(gbuf)) > 0) {
            gzOut.write(gbuf, 0, gn);
        }
        gis.close();
        System.out.println("gzip_ok=" + eq(gzOut.toByteArray(), data));
        System.out.println("gzip_output_len=" + gzOut.size());

        // ZipOutputStream / ZipInputStream round-trip.
        byte[] second = asciiBytes("second entry");
        ByteArrayOutputStream zipBytes = new ByteArrayOutputStream();
        ZipOutputStream zos = new ZipOutputStream(zipBytes);
        zos.putNextEntry(new ZipEntry("first.txt"));
        zos.write(data);
        zos.closeEntry();
        zos.putNextEntry(new ZipEntry("second.txt"));
        zos.write(second);
        zos.closeEntry();
        zos.close();

        ZipInputStream zis = new ZipInputStream(new ByteArrayInputStream(zipBytes.toByteArray()));
        ZipEntry entry;
        int entryCount = 0;
        while ((entry = zis.getNextEntry()) != null) {
            ByteArrayOutputStream entryOut = new ByteArrayOutputStream();
            int n;
            while ((n = zis.read(gbuf)) > 0) {
                entryOut.write(gbuf, 0, n);
            }
            byte[] entryBytes = entryOut.toByteArray();
            System.out.println("zip_entry=" + entry.getName()
                    + " len=" + entryBytes.length
                    + " crc=" + entry.getCrc()
                    + " ascii=" + ascii(entryBytes));
            entryCount++;
        }
        zis.close();
        System.out.println("zip_entry_count=" + entryCount);
        System.out.println("done");
    }
}
