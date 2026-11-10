import android.os.Parcel;
import android.view.SurfaceControl;

/**
 * Java-level acceptance for the T-28 `view.surface` natives.
 *
 * <p>This uses only the public API-34 framework surface classes: the program is
 * compiled against {@code android.jar} and run on the real boot-classpath DEX by
 * the Rust harness. Nothing here is a mock call — {@code build()},
 * {@code apply()}, {@code writeToParcel} and {@code createFromParcel} are the
 * framework's own bytecode, and they reach the Rust natives in
 * {@code natives/fwk_surface.rs}.
 */
public final class SurfaceNativesMain {
    private SurfaceNativesMain() {}

    public static void main(String[] args) {
        SurfaceControl sc =
                new SurfaceControl.Builder()
                        .setName("acos-surface-test")
                        .setBufferSize(120, 240)
                        .build();
        System.out.println("valid=" + sc.isValid());

        SurfaceControl.Transaction t = new SurfaceControl.Transaction();
        t.setPosition(sc, 3.0f, 4.0f);
        t.setAlpha(sc, 0.5f);
        t.setLayer(sc, 7);
        t.apply();
        System.out.println("applied=1");

        // Parcel round-trip: write the transaction, read it back, write the
        // read copy, and compare the two marshalled byte streams.
        Parcel p = Parcel.obtain();
        t.writeToParcel(p, 0);
        p.setDataPosition(0);
        SurfaceControl.Transaction t2 = SurfaceControl.Transaction.CREATOR.createFromParcel(p);
        Parcel p2 = Parcel.obtain();
        t2.writeToParcel(p2, 0);
        System.out.println(
                "parcel_stable=" + java.util.Arrays.equals(p.marshall(), p2.marshall()));

        // A second applied transaction must bump the compositor generation
        // again (one apply == one generation).
        SurfaceControl.Transaction t3 = new SurfaceControl.Transaction();
        t3.setAlpha(sc, 0.25f);
        t3.apply();
        System.out.println("applied=2");

        // A parcel-read wrapper is an independent native reference: releasing
        // the original must not disconnect the layer under it (AOSP
        // `sp<SurfaceControl>` semantics). The alias stays applicable.
        Parcel handleParcel = Parcel.obtain();
        sc.writeToParcel(handleParcel, 0);
        handleParcel.setDataPosition(0);
        SurfaceControl stale = SurfaceControl.CREATOR.createFromParcel(handleParcel);
        sc.release();
        SurfaceControl.Transaction t4 = new SurfaceControl.Transaction();
        t4.setAlpha(stale, 0.1f);
        try {
            t4.apply();
            System.out.println("stale=no-exception");
        } catch (Throwable e) {
            System.out.println("stale=" + e.getClass().getName());
        }

        System.out.println("done");
    }
}
