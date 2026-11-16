package dev.acos.fixtures.locationprobe;

import android.app.Activity;
import android.content.Context;
import android.location.Geocoder;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.widget.TextView;

import java.util.List;
import java.util.Locale;
import java.util.function.Consumer;

/**
 * Location fixture: the smallest app that exercises the `location` service module
 * through the real framework client classes, and shows on screen what it got.
 *
 * <p>Observable behaviour (each line is assertable text):
 * <ul>
 *   <li>{@code state=} the outcome of the first API calls: {@code ready},
 *       {@code permission-denied: ...} or {@code unavailable: ...}</li>
 *   <li>{@code last=} {@code getLastKnownLocation("gps")} (lat/lng to 4 decimals
 *       plus the accuracy), or {@code none}</li>
 *   <li>{@code fix=} the newest delivered update: {@code lat,lng provider=gps
 *       count=N}, or {@code none}</li>
 * </ul>
 *
 * <p>Every callback runs on the main looper, so a UI assertion after
 * {@code service_ctl} sees the screen the app actually updated.
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosLocationProbe";

    private TextView stateView;
    private TextView lastView;
    private TextView fixView;
    private LocationManager manager;
    private LocationListener listener;
    private int fixCount;
    private String newest = "none";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.main);
        stateView = (TextView) findViewById(R.id.state);
        lastView = (TextView) findViewById(R.id.last);
        fixView = (TextView) findViewById(R.id.fix);
        manager = (LocationManager) getSystemService(Context.LOCATION_SERVICE);
        if (manager == null) {
            stateView.setText("state=unavailable: no LocationManager");
            return;
        }
        ((android.widget.Button) findViewById(R.id.check)).setOnClickListener(v -> probe());
        probe();
    }

    /**
     * Every location API call the probe makes, in one place so the "Check again"
     * button re-runs exactly the launch path (a session can detach the module
     * live; the app's next call must show the honest state).
     */
    private void probe() {
        // A re-check starts from an empty screen: after a live module detach the
        // old fix must not stay on screen looking like a working phone.
        lastView.setText("last=none");
        fixView.setText("fix=none");
        fixCount = 0;
        newest = "none";
        try {
            reportProviders();
            reportLastKnown();
            registerUpdates();
            requestCurrentLocation();
            reportGeocoder();
        } catch (SecurityException denied) {
            // The framework's own path when neither location permission is
            // granted (LocationManagerService.enforceLocationPermission).
            stateView.setText("state=permission-denied: " + denied.getMessage());
            Log.i(TAG, "permission denied: " + denied.getMessage());
        } catch (RuntimeException failure) {
            // The honest missing path: the service module is off and the name
            // answered a structured error (or there is no service at all).
            stateView.setText("state=unavailable: " + failure.getClass().getSimpleName()
                    + ": " + failure.getMessage());
            Log.i(TAG, "unavailable: " + failure);
        }
    }

    private void reportProviders() {
        List<String> providers = manager.getAllProviders();
        boolean enabled = manager.isLocationEnabled();
        boolean gps = manager.isProviderEnabled(LocationManager.GPS_PROVIDER);
        stateView.setText("state=ready providers=" + join(providers)
                + " enabled=" + enabled + " gps=" + gps);
        Log.i(TAG, "providers=" + providers + " enabled=" + enabled + " gps=" + gps);
    }

    private void reportLastKnown() {
        Location last = manager.getLastKnownLocation(LocationManager.GPS_PROVIDER);
        if (last == null) {
            lastView.setText("last=none");
        } else {
            lastView.setText("last=" + format(last) + " accuracy=" + last.getAccuracy());
        }
        Log.i(TAG, "last=" + last);
    }

    private void registerUpdates() {
        listener = new LocationListener() {
            @Override
            public void onLocationChanged(Location location) {
                fixCount++;
                newest = format(location) + " provider=" + location.getProvider();
                fixView.setText("fix=" + newest + " count=" + fixCount);
                // Re-read the cached fix: a delivered update also refreshes
                // `getLastKnownLocation`, exactly as a provider's last fix does.
                reportLastKnown();
                Log.i(TAG, "fix #" + fixCount + " " + location);
            }

            @Override
            public void onProviderEnabled(String provider) {
                Log.i(TAG, "provider enabled " + provider);
            }

            @Override
            public void onProviderDisabled(String provider) {
                Log.i(TAG, "provider disabled " + provider);
            }
        };
        // 200 ms / 0 m: the module's scripted feed decides the real timing, and a
        // smaller interval keeps the pattern close to `play_track`'s.
        manager.requestLocationUpdates(LocationManager.GPS_PROVIDER, 200L, 0f, listener,
                Looper.getMainLooper());
        Log.i(TAG, "registered for gps updates");
    }

    private void requestCurrentLocation() {
        CancellationSignal signal = new CancellationSignal();
        Consumer<Location> consumer = location -> {
            if (location == null) {
                Log.i(TAG, "getCurrentLocation answered null");
            } else {
                newest = format(location) + " provider=" + location.getProvider();
                fixView.setText("fix=" + newest + " count=" + fixCount);
                Log.i(TAG, "current location " + location);
            }
        };
        manager.getCurrentLocation(LocationManager.GPS_PROVIDER, signal,
                getMainExecutor(), consumer);
    }

    private void reportGeocoder() {
        boolean present = Geocoder.isPresent();
        Log.i(TAG, "geocoder present=" + present);
        final boolean[] answered = {false};
        new Geocoder(this, Locale.US).getFromLocation(37.4220, -122.0841, 3, addresses -> {
            answered[0] = true;
            Log.i(TAG, "reverse geocode results=" + (addresses == null ? -1 : addresses.size())
                    + " first=" + (addresses == null || addresses.isEmpty()
                            ? null : addresses.get(0).getAddressLine(0)));
            if (addresses != null && !addresses.isEmpty()) {
                stateView.setText(stateView.getText() + " geocode="
                        + addresses.get(0).getAddressLine(0));
            }
        });
        new Handler(getMainLooper()).postDelayed(() -> {
            if (!answered[0]) {
                Log.i(TAG, "reverse geocode unanswered after 2000 ms");
            }
        }, 2000L);
    }

    private static String format(Location location) {
        return String.format(Locale.US, "%.4f,%.4f", location.getLatitude(),
                location.getLongitude());
    }

    private static String join(List<String> values) {
        StringBuilder builder = new StringBuilder();
        for (String value : values) {
            if (builder.length() > 0) {
                builder.append(',');
            }
            builder.append(value);
        }
        return builder.toString();
    }
}
