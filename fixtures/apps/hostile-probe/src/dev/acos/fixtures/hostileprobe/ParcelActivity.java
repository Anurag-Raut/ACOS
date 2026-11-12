package dev.acos.fixtures.hostileprobe;

import android.os.Bundle;
import android.view.View;

/**
 * W44 hostile fixture: sends the containment-side hostile parcels a third-party
 * APK could send, from the real guest over the real binder.
 *
 * <ul>
 *   <li>{@code setTransactionState} with 257 composer states (layer-count bomb);</li>
 *   <li>{@code setTransactionState} with a {@code GraphicBuffer} geometry of
 *       {@code 0xffffffff x 0xffffffff}, stride {@code 0xffffffff};</li>
 *   <li>{@code addToDisplayWithoutInputChannel} with an {@code out float[]}
 *       length of {@code i32::MAX} (the S7 §4.2 bomb).</li>
 * </ul>
 *
 * <p>Each must be refused by the host with a structured error; the guest stays
 * alive and the run must not pass on any of it. It prints
 * {@code ACOSHOSTILE {"probe":...}} for each attempt so the evidence says
 * whether the transaction was even delivered (a guest-side reflection failure
 * is not host containment evidence).
 */
public class ParcelActivity extends MainActivity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(new RedSurface(this));
        getWindow().getDecorView().postDelayed(this::fire, 400);
    }

    private void fire() {
        HostileParcels.layerCountBomb();
        HostileParcels.geometryBomb();
        HostileParcels.sizeCompatScaleBomb(getCurrentFocus() == null
                ? getWindow().getDecorView() : getCurrentFocus());
    }

    @Override
    protected void onResume() {
        // Keep the forged lines: this activity is about containment, not the
        // verdict channel, but a guest can do both at once.
        super.onResume();
    }
}
