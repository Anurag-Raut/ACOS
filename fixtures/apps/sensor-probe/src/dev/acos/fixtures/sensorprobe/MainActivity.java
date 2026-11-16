// DEVICE (MODSENS): the real-app proof for the `sensors` service module.
//
// It is a plain framework app: no ACOS API, no debug hook. It does exactly what a
// third-party app does with Android's sensor stack, and the test asserts on what
// it printed:
//
//   * `getSensorList(TYPE_ALL)` — the native `sensorservice` sensor table, so the
//     count is what the framework sees (0 when the module is off, 7 when
//     `sensors:scripted` is attached);
//   * a `registerListener` on the accelerometer and the light sensor at
//     `SENSOR_DELAY_GAME` (20 ms), so events travel the whole chain
//     `SensorEventConnection` → `BitTube` → `SensorEventQueue` → this callback;
//   * a shake counter that counts sign flips of the x axis above 2.3 m/s² (what a
//     real shake detector does), so a scripted `shake` timeline is observably
//     different from a static value, and a "shake ended" state once the flips
//     stop for half a second;
//   * live markers (`accel_seen`, `light_seen`) that become visible only after
//     the matching event was processed, so a scenario waits event-driven instead
//     of sleeping.
//
// Every line is `String.format(Locale.US, ...)` so the text an agent asserts on
// does not depend on the device locale.
package dev.acos.fixtures.sensorprobe;

import android.app.Activity;
import android.hardware.Sensor;
import android.hardware.SensorEvent;
import android.hardware.SensorEventListener;
import android.hardware.SensorManager;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

import java.util.List;
import java.util.Locale;

public class MainActivity extends Activity implements SensorEventListener {
    private static final String TAG = "SensorProbe";
    /** x-axis magnitude above which an event counts towards the shake counter. */
    private static final float SHAKE_THRESHOLD = 2.3f;
    /** Quiet time after the last flip that says "the shake is over". */
    private static final long SHAKE_IDLE_MS = 500;

    private final Handler handler = new Handler(Looper.getMainLooper());

    private SensorManager manager;
    private TextView countView;
    private TextView namesView;
    private TextView accelView;
    private TextView lightView;
    private TextView shakeView;
    private TextView accelSeen;
    private TextView lightSeen;
    private TextView shakeState;

    private int shakeCount;
    private int lastSign;
    private int accelEvents;
    private int lightEvents;
    private int flipsAtLastSchedule;

    private Sensor accelerometer;
    private Sensor light;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.main);
        countView = findViewById(R.id.sensor_count);
        namesView = findViewById(R.id.sensor_names);
        accelView = findViewById(R.id.accel);
        lightView = findViewById(R.id.light);
        shakeView = findViewById(R.id.shake);
        accelSeen = findViewById(R.id.accel_seen);
        lightSeen = findViewById(R.id.light_seen);
        shakeState = findViewById(R.id.shake_state);
        Button rescan = findViewById(R.id.rescan);
        rescan.setOnClickListener(view -> describeSensors());

        manager = (SensorManager) getSystemService(SENSOR_SERVICE);
        describeSensors();

        accelerometer = manager.getDefaultSensor(Sensor.TYPE_ACCELEROMETER);
        light = manager.getDefaultSensor(Sensor.TYPE_LIGHT);
        Log.i(TAG, "accelerometer=" + accelerometer + " light=" + light);
    }

    /** The native sensor table, as the framework reports it right now. */
    private void describeSensors() {
        List<Sensor> sensors = manager.getSensorList(Sensor.TYPE_ALL);
        StringBuilder names = new StringBuilder();
        for (Sensor sensor : sensors) {
            if (names.length() > 0) {
                names.append(' ');
            }
            names.append(shortName(sensor.getType()));
        }
        countView.setText(String.format(Locale.US, "sensors: %d", sensors.size()));
        namesView.setText(names.toString());
        Log.i(TAG, "sensors=" + sensors.size() + " [" + names + "]");
    }

    private static String shortName(int type) {
        switch (type) {
            case Sensor.TYPE_ACCELEROMETER:
                return "accelerometer";
            case Sensor.TYPE_MAGNETIC_FIELD:
                return "magnetometer";
            case Sensor.TYPE_GYROSCOPE:
                return "gyroscope";
            case Sensor.TYPE_LIGHT:
                return "light";
            case Sensor.TYPE_PROXIMITY:
                return "proximity";
            case Sensor.TYPE_ROTATION_VECTOR:
                return "rotation_vector";
            case Sensor.TYPE_STEP_COUNTER:
                return "step_counter";
            default:
                return "type" + type;
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        if (accelerometer != null) {
            manager.registerListener(this, accelerometer, SensorManager.SENSOR_DELAY_GAME);
        }
        if (light != null) {
            manager.registerListener(this, light, SensorManager.SENSOR_DELAY_GAME);
        }
    }

    @Override
    protected void onPause() {
        super.onPause();
        manager.unregisterListener(this);
    }

    @Override
    public void onSensorChanged(SensorEvent event) {
        if (event.sensor.getType() == Sensor.TYPE_ACCELEROMETER) {
            onAccelerometer(event);
        } else if (event.sensor.getType() == Sensor.TYPE_LIGHT) {
            onLight(event);
        }
    }

    private void onAccelerometer(SensorEvent event) {
        accelEvents++;
        float x = event.values[0];
        float y = event.values[1];
        float z = event.values[2];
        if (Math.abs(x) > SHAKE_THRESHOLD) {
            int sign = x > 0 ? 1 : -1;
            if (lastSign != 0 && sign != lastSign) {
                shakeCount++;
                scheduleShakeEnd();
            }
            lastSign = sign;
        }
        // Only touch the view when the text actually changes: a 50 Hz stream of
        // identical samples must not invalidate the window 50 times a second
        // (`setText` with an equal string still schedules a redraw), which would
        // also keep every strict settle from settling.
        setTextIfChanged(accelView, String.format(Locale.US, "accel: %.1f %.1f %.1f", x, y, z));
        setTextIfChanged(shakeView, String.format(Locale.US, "shake: %d", shakeCount));
        if (Math.abs(x) > 1.0f && accelSeen.getVisibility() != View.VISIBLE) {
            accelSeen.setVisibility(View.VISIBLE);
        }
    }

    /**
     * A shake detector reports that the shake ended: no flip for
     * {@link #SHAKE_IDLE_MS} means this scripted timeline is done, and the count
     * shown next to it is final.
     */
    private void scheduleShakeEnd() {
        final int scheduledAt = shakeCount;
        if (scheduledAt == flipsAtLastSchedule) {
            return;
        }
        flipsAtLastSchedule = scheduledAt;
        handler.postDelayed(() -> {
            if (shakeCount == scheduledAt) {
                shakeState.setText(String.format(Locale.US, "shake ended: %d", shakeCount));
                shakeState.setVisibility(View.VISIBLE);
                Log.i(TAG, "shake ended after " + shakeCount + " flips");
            }
        }, SHAKE_IDLE_MS);
    }

    private void onLight(SensorEvent event) {
        lightEvents++;
        float lux = event.values[0];
        setTextIfChanged(lightView, String.format(Locale.US, "light: %.0f", lux));
        if (lux > 1000.0f && lightSeen.getVisibility() != View.VISIBLE) {
            lightSeen.setVisibility(View.VISIBLE);
        }
    }

    private static void setTextIfChanged(TextView view, String text) {
        if (!text.contentEquals(view.getText())) {
            view.setText(text);
        }
    }

    @Override
    public void onAccuracyChanged(Sensor sensor, int accuracy) {
        // Nothing to show: ACOS's scripted feed reports SENSOR_STATUS_ACCURACY_HIGH.
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        Log.i(TAG, "events accel=" + accelEvents + " light=" + lightEvents
                + " shakes=" + shakeCount + " at " + SystemClock.uptimeMillis());
    }
}
