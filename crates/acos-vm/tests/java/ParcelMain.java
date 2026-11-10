import android.os.Bundle;
import android.os.Parcel;
import android.os.ParcelFileDescriptor;
import java.util.ArrayList;
import java.util.List;

/**
 * Java-level acceptance for the {@code android.os.Parcel} natives (T-18).
 *
 * <p>Runs against the real Android 14 core library + framework DEX in
 * {@code --corelib real} mode. Every assertion below travels through the Java
 * {@code Parcel} wrapper into the Rust codec, so a wrong native signature or a
 * wrong endianness/padding choice fails here.
 *
 * <p>Covered: {@code Parcel.obtain}/{@code recycle}, int/long/float/double,
 * String (String16), String[] (String16[]), byte[], {@code Bundle} (int +
 * String + Integer ArrayList), {@code List} via {@code writeList/readList},
 * {@code appendFrom}, {@code marshall}/{@code unmarshall}, and the interface
 * token magic ({@code "SYST"} = {@code 0x53595354}).
 */
public final class ParcelMain {
    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    public static void main(String[] args) {
        primitives();
        bundleCase();
        listCase();
        appendFromCase();
        marshallCase();
        fileDescriptorCase();
        interfaceTokenCase();
        System.out.println("done");
    }

    /** Write scalars/strings/arrays, rewind, read back identical. */
    static void primitives() {
        Parcel p = Parcel.obtain();
        p.writeInt(0x12345678);
        p.writeLong(0x1122334455667788L);
        p.writeFloat(1.5f);
        p.writeDouble(2.25);
        p.writeString("hello");
        p.writeStringArray(new String[]{"alpha", "beta", "gamma"});
        p.writeByteArray(new byte[]{1, 2, 3, 4, 5});
        int size = p.dataSize();
        check(size > 0, "dataSize must be positive");
        check(p.dataPosition() == size, "position at end before rewind");

        p.setDataPosition(0);
        check(p.readInt() == 0x12345678, "readInt");
        check(p.readLong() == 0x1122334455667788L, "readLong");
        check(p.readFloat() == 1.5f, "readFloat");
        check(p.readDouble() == 2.25, "readDouble");
        check("hello".equals(p.readString()), "readString");
        String[] sa = p.createStringArray();
        check(sa != null && sa.length == 3, "createStringArray length");
        check("alpha".equals(sa[0]) && "beta".equals(sa[1]) && "gamma".equals(sa[2]),
                "createStringArray values");
        byte[] ba = new byte[5];
        p.readByteArray(ba);
        check(ba[0] == 1 && ba[4] == 5, "readByteArray");
        check(p.dataPosition() == size, "position at end after read");
        check(p.dataAvail() == 0, "dataAvail after read");

        // writeByteArray(null) writes -1, createByteArray reads null back.
        Parcel n = Parcel.obtain();
        n.writeByteArray(null);
        n.setDataPosition(0);
        check(n.createByteArray() == null, "null byte array round-trip");
        n.recycle();

        p.recycle();
        System.out.println("primitives=ok");
    }

    /** A Bundle with an int, a String and an Integer ArrayList. */
    static void bundleCase() {
        Bundle b = new Bundle();
        b.putInt("i", 7);
        b.putString("s", "hi");
        ArrayList<Integer> list = new ArrayList<>();
        list.add(10);
        list.add(20);
        b.putIntegerArrayList("l", list);

        Parcel p = Parcel.obtain();
        b.writeToParcel(p, 0);
        int size = p.dataSize();
        check(size > 0, "bundle parcel non-empty");
        p.setDataPosition(0);
        Bundle b2 = new Bundle();
        b2.readFromParcel(p);
        check(b2.getInt("i") == 7, "bundle int");
        check("hi".equals(b2.getString("s")), "bundle string");
        ArrayList<Integer> l2 = b2.getIntegerArrayList("l");
        check(l2 != null && l2.size() == 2, "bundle list size");
        check(l2.get(0).intValue() == 10 && l2.get(1).intValue() == 20, "bundle list values");
        check(p.dataPosition() == size, "bundle position at end");
        p.recycle();
        System.out.println("bundle=7,hi,[10,20]");
    }

    /** List written with writeList/readList (Integer, String, Boolean). */
    static void listCase() {
        List<Object> list = new ArrayList<>();
        list.add(Integer.valueOf(1));
        list.add("two");
        list.add(Boolean.TRUE);

        Parcel p = Parcel.obtain();
        p.writeList(list);
        int size = p.dataSize();
        p.setDataPosition(0);
        List<Object> out = new ArrayList<>();
        p.readList(out, null);
        check(out.size() == 3, "list size");
        check(Integer.valueOf(1).equals(out.get(0)), "list int");
        check("two".equals(out.get(1)), "list string");
        check(Boolean.TRUE.equals(out.get(2)), "list boolean");
        check(p.dataPosition() == size, "list position at end");
        p.recycle();
        System.out.println("list=1,two,true");
    }

    /**
     * A file descriptor written to a parcel and read back must be a *fully
     * constructed* {@code java.io.FileDescriptor}.
     *
     * <p>{@code ParcelFileDescriptor.detachFd()} calls
     * {@code IoUtils.acquireRawFd(mFd)} which calls
     * {@code mFd.release$()}. {@code release$()} synchronises on the private
     * final {@code FileDescriptor.releaseLock} object initialised by
     * {@code FileDescriptor.<init>()}. A descriptor materialised by the parcel
     * native without running that constructor has a null {@code releaseLock},
     * so the monitor-enter throws {@code NullPointerException: monitor-enter on
     * null}. Regression guard for that path (T-27 Settings provider).
     */
    static void fileDescriptorCase() {
        Parcel p = Parcel.obtain();
        try {
            // Writing a descriptor and reading it back makes the parcel native
            // materialise a fresh java.io.FileDescriptor.
            p.writeFileDescriptor(java.io.FileDescriptor.in);
            int size = p.dataSize();
            check(size > 0, "fd parcel non-empty");
            p.setDataPosition(0);
            ParcelFileDescriptor read = p.readFileDescriptor();
            check(read != null, "readFileDescriptor");
            // detachFd -> IoUtils.acquireRawFd -> FileDescriptor.release$().
            int fd = read.detachFd();
            check(fd >= 0, "detachFd returned a live descriptor");
        } finally {
            p.recycle();
        }
        System.out.println("fd=ok");
    }

    /** appendFrom copies a whole parcel's bytes onto another. */
    static void appendFromCase() {
        Parcel a = Parcel.obtain();
        a.writeInt(111);
        a.writeString("one");
        Parcel b = Parcel.obtain();
        b.appendFrom(a, 0, a.dataSize());
        int size = b.dataSize();
        check(size == a.dataSize(), "appendFrom copied all bytes");
        b.setDataPosition(0);
        check(b.readInt() == 111, "append int");
        check("one".equals(b.readString()), "append string");
        check(b.dataPosition() == size, "append position at end");
        a.recycle();
        b.recycle();
        System.out.println("append=111,one");
    }

    /** marshall() then unmarshall() reproduces the same bytes. */
    static void marshallCase() {
        Parcel p = Parcel.obtain();
        p.writeInt(0x0badf00d);
        p.writeString("marshalled");
        byte[] data = p.marshall();
        check(data != null && data.length == p.dataSize(), "marshall length equals dataSize");
        Parcel q = Parcel.obtain();
        q.unmarshall(data, 0, data.length);
        check(q.dataSize() == p.dataSize(), "unmarshall restores size");
        q.setDataPosition(0);
        check(q.readInt() == 0x0badf00d, "unmarshall int");
        check("marshalled".equals(q.readString()), "unmarshall string");
        p.recycle();
        q.recycle();
        System.out.println("marshall=ok");
    }

    /** The interface-token magic on the wire is "SYST" (0x53595354). */
    static void interfaceTokenCase() {
        Parcel p = Parcel.obtain();
        p.writeInterfaceToken("com.example.ITest");
        // Layout: int32 policy|0x80000000, int32 workSourceUid, int32 header, String16.
        p.setDataPosition(8);
        int header = p.readInt();
        check(header == 0x53595354, "interface token header must be SYST, got 0x"
                + Integer.toHexString(header));
        p.setDataPosition(0);
        p.enforceInterface("com.example.ITest");
        p.recycle();
        System.out.println("token-header=0x53595354");
    }
}
