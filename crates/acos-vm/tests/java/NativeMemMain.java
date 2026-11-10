import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.lang.reflect.Field;
import java.nio.ByteBuffer;
import java.nio.MappedByteBuffer;
import java.nio.channels.FileChannel;

import libcore.io.Memory;
import sun.misc.Unsafe;

/**
 * Java-level acceptance for the ACOS native-memory substrate.
 *
 * Runs on the real Android 14 boot classpath (`--corelib real`) so the real
 * `java.nio.DirectByteBuffer`/`Bits`, `sun.misc.Unsafe` and `libcore.io.Memory`
 * bytecode execute on top of the Rust-owned region table:
 *
 *  - `ByteBuffer.allocateDirect` + typed put/get;
 *  - `Unsafe.allocateMemory`/`put*`/`get*`/`setMemory`/`copyMemory`/`freeMemory`;
 *  - `libcore.io.Memory.peekByte`/`pokeByte`/`peekInt`/`pokeInt`/byte arrays;
 *  - a read-only `FileChannel.map` over a VFS-backed file, including a
 *    sub-range map;
 *  - a freed region raises a structured Java exception (not a crash).
 *
 * Output is deterministic hex so the Rust harness can compare exactly.
 */
public class NativeMemMain {
    public static void main(String[] args) throws Exception {
        // 1. ByteBuffer.allocateDirect: the DirectByteBuffer + Bits path.
        ByteBuffer b = ByteBuffer.allocateDirect(64);
        b.putInt(0x01020304);
        b.putLong(0x1122334455667788L);
        b.putShort((short) 0x0a0b);
        b.put((byte) 0x7f);
        b.putDouble(1.5d);
        b.putFloat(2.5f);
        b.putChar('A');
        b.flip();
        System.out.println("direct.int=" + Integer.toHexString(b.getInt()));
        System.out.println("direct.long=" + Long.toHexString(b.getLong()));
        System.out.println("direct.short=" + Integer.toHexString(b.getShort() & 0xffff));
        System.out.println("direct.byte=" + Integer.toHexString(b.get() & 0xff));
        System.out.println("direct.double=" + Double.toString(b.getDouble()));
        System.out.println("direct.float=" + Float.toString(b.getFloat()));
        System.out.println("direct.char=" + Integer.toHexString(b.getChar()));

        Unsafe u = unsafe();
        long addr = u.allocateMemory(64);
        u.putLong(addr, 0x0102030405060708L);
        u.putInt(addr + 8, 0xdeadbeef);
        u.putByte(addr + 12, (byte) 0x9a);
        u.putShort(addr + 16, (short) 0x0f0e);
        u.putFloat(addr + 20, 3.25f);
        u.putDouble(addr + 24, 6.5d);
        System.out.println("unsafe.long=" + Long.toHexString(u.getLong(addr)));
        System.out.println("unsafe.int=" + Integer.toHexString(u.getInt(addr + 8)));
        System.out.println("unsafe.byte=" + Integer.toHexString(u.getByte(addr + 12) & 0xff));
        System.out.println("unsafe.short=" + Integer.toHexString(u.getShort(addr + 16) & 0xffff));
        System.out.println("unsafe.float=" + Float.toString(u.getFloat(addr + 20)));
        System.out.println("unsafe.double=" + Double.toString(u.getDouble(addr + 24)));

        // setMemory / copyMemory.
        u.setMemory(addr + 32, 8, (byte) 0x5a);
        System.out.println("unsafe.set=" + Long.toHexString(u.getLong(addr + 32)));
        long copy = u.allocateMemory(64);
        u.copyMemory(addr, copy, 64);
        System.out.println("unsafe.copy=" + Long.toHexString(u.getLong(copy + 32)));

        // 2. libcore.io.Memory peek/poke on the same region.
        Memory.pokeInt(copy, 0x7f6e5d4c, false);
        System.out.println("mem.int=" + Integer.toHexString(Memory.peekInt(copy, false)));
        Memory.pokeByte(copy + 4, (byte) 0x33);
        System.out.println("mem.byte=" + Integer.toHexString(Memory.peekByte(copy + 4) & 0xff));
        byte[] src = new byte[] { 1, 2, 3, 4, 5, 6 };
        Memory.pokeByteArray(copy + 8, src, 0, src.length);
        byte[] dst = new byte[src.length];
        Memory.peekByteArray(copy + 8, dst, 0, dst.length);
        System.out.println("mem.bytes=" + Integer.toHexString(dst[0] & 0xff)
                + Integer.toHexString(dst[3] & 0xff) + Integer.toHexString(dst[5] & 0xff));

        // 3. A freed region must raise a structured Java exception.
        long freed = u.allocateMemory(8);
        u.freeMemory(freed);
        try {
            u.getLong(freed);
            System.out.println("freed=none");
        } catch (Throwable t) {
            System.out.println("freed=" + t.getClass().getSimpleName());
        }

        // 4. Read-only FileChannel.map over a VFS-backed file.
        File dir = new File("/data/user/0");
        dir.mkdirs();
        File file = new File(dir, "acos-native-mem.bin");
        byte[] payload = new byte[] {
            0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x00,
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x01
        };
        try (FileOutputStream fos = new FileOutputStream(file)) {
            fos.write(payload);
        }
        try (FileInputStream fis = new FileInputStream(file)) {
            FileChannel ch = fis.getChannel();
            MappedByteBuffer mb = ch.map(FileChannel.MapMode.READ_ONLY, 0, payload.length);
            byte[] got = new byte[payload.length];
            mb.get(got);
            System.out.println("map.full=" + Integer.toHexString(got[0] & 0xff)
                    + Integer.toHexString(got[7] & 0xff)
                    + Integer.toHexString(got[15] & 0xff));
            MappedByteBuffer sub = ch.map(FileChannel.MapMode.READ_ONLY, 4, 8);
            byte[] subGot = new byte[8];
            sub.get(subGot);
            System.out.println("map.sub=" + Integer.toHexString(subGot[0] & 0xff)
                    + Integer.toHexString(subGot[3] & 0xff)
                    + Integer.toHexString(subGot[7] & 0xff));
            System.out.println("map.size=" + mb.capacity() + "," + sub.capacity());
        }
        try (FileInputStream ignored = new FileInputStream(file)) {
            System.out.println("map.remain=" + ignored.available());
        }

        u.freeMemory(addr);
        u.freeMemory(copy);
        file.delete();
        System.out.println("done");
    }

    private static Unsafe unsafe() throws Exception {
        Field f = Unsafe.class.getDeclaredField("theUnsafe");
        f.setAccessible(true);
        return (Unsafe) f.get(null);
    }
}
