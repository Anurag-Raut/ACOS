package dev.acos.fixtures.cameraprobe;

import android.app.Activity;
import android.content.Context;
import android.content.pm.PackageManager;
import android.graphics.ImageFormat;
import android.graphics.Rect;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CameraMetadata;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.util.Size;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import java.util.Arrays;
import java.util.Locale;

/**
 * The `hw.camera` proof fixture (CAMERA2 Slice 1).
 *
 * <p>Exercises the real framework {@link CameraManager}, {@link CameraCharacteristics},
 * and {@link StreamConfigurationMap} (backed by {@code libcamera_client.so} and
 * {@code libcamera_metadata.so}): camera enumeration, characteristics parsing,
 * unknown-id error handling, {@code CAMERA} permission enforcement on
 * {@code openCamera}, and the {@code off} tombstone path.
 */
public final class MainActivity extends Activity {
    private static final String TAG = "cameraprobe";

    private TextView status;
    private CameraManager cameraManager;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(24, 24, 24, 24);

        TextView title = new TextView(this);
        title.setId(R.id.title);
        title.setTextSize(20);
        title.setText("CAMERAPROBE");
        root.addView(title);

        cameraManager = (CameraManager) getSystemService(Context.CAMERA_SERVICE);

        root.addView(button("List", v -> listCameras()));
        root.addView(button("Chars", v -> queryCharacteristics()));
        root.addView(button("Unknown", v -> queryUnknown()));
        root.addView(button("Open", v -> openCamera()));

        status = new TextView(this);
        status.setId(R.id.status);
        status.setTextIsSelectable(true);
        root.addView(status);

        ScrollView scroll = new ScrollView(this);
        scroll.addView(root);
        setContentView(scroll);

        PackageManager pm = getPackageManager();
        boolean featCam = pm.hasSystemFeature(PackageManager.FEATURE_CAMERA);
        boolean featAny = pm.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY);
        log("READY mgr=" + (cameraManager != null ? "present" : "null")
                + " feat_cam=" + featCam
                + " feat_any=" + featAny);
    }

    private Button button(String label, View.OnClickListener onClick) {
        Button b = new Button(this);
        b.setText(label);
        b.setGravity(Gravity.CENTER);
        b.setOnClickListener(onClick);
        return b;
    }

    private void log(String line) {
        Log.i(TAG, line);
        CharSequence prev = status.getText();
        status.setText(prev == null || prev.length() == 0 ? line : prev + "\n" + line);
    }

    private void listCameras() {
        try {
            String[] ids = cameraManager.getCameraIdList();
            log("LIST count=" + ids.length + " ids=" + Arrays.toString(ids));
        } catch (Throwable t) {
            log("LIST error=" + t.getClass().getSimpleName() + ": " + t.getMessage());
        }
    }

    private void queryCharacteristics() {
        try {
            CameraCharacteristics cc = cameraManager.getCameraCharacteristics("0");
            Integer facing = cc.get(CameraCharacteristics.LENS_FACING);
            Integer orient = cc.get(CameraCharacteristics.SENSOR_ORIENTATION);
            Integer level = cc.get(CameraCharacteristics.INFO_SUPPORTED_HARDWARE_LEVEL);
            Size arraySize = cc.get(CameraCharacteristics.SENSOR_INFO_PIXEL_ARRAY_SIZE);
            Rect active = cc.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE);
            Boolean flash = cc.get(CameraCharacteristics.FLASH_INFO_AVAILABLE);
            float[] focals = cc.get(CameraCharacteristics.LENS_INFO_AVAILABLE_FOCAL_LENGTHS);
            int[] caps = cc.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES);
            StreamConfigurationMap map =
                    cc.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);

            String facingStr = facing == null ? "null"
                    : (facing == CameraMetadata.LENS_FACING_BACK ? "BACK"
                    : facing == CameraMetadata.LENS_FACING_FRONT ? "FRONT" : String.valueOf(facing));
            String levelStr = level == null ? "null"
                    : (level == CameraMetadata.INFO_SUPPORTED_HARDWARE_LEVEL_LIMITED ? "LIMITED"
                    : level == CameraMetadata.INFO_SUPPORTED_HARDWARE_LEVEL_FULL ? "FULL"
                    : level == CameraMetadata.INFO_SUPPORTED_HARDWARE_LEVEL_LEGACY ? "LEGACY"
                    : String.valueOf(level));
            String arrayStr = arraySize == null ? "null"
                    : arraySize.getWidth() + "x" + arraySize.getHeight();
            String activeStr = active == null ? "null"
                    : active.width() + "x" + active.height();
            String focalStr = (focals != null && focals.length > 0)
                    ? String.format(Locale.US, "%.2f", focals[0])
                    : "none";

            int jpegCount = 0;
            String jpegMax = "none";
            int yuvCount = 0;
            if (map != null) {
                Size[] jpegs = map.getOutputSizes(ImageFormat.JPEG);
                if (jpegs != null && jpegs.length > 0) {
                    jpegCount = jpegs.length;
                    jpegMax = jpegs[0].getWidth() + "x" + jpegs[0].getHeight();
                }
                Size[] yuvs = map.getOutputSizes(ImageFormat.YUV_420_888);
                if (yuvs != null) {
                    yuvCount = yuvs.length;
                }
            }

            log("CHARS facing=" + facingStr
                    + " orient=" + orient
                    + " level=" + levelStr
                    + " array=" + arrayStr
                    + " active=" + activeStr
                    + " flash=" + flash
                    + " focal=" + focalStr
                    + " jpeg=" + jpegCount + "(" + jpegMax + ")"
                    + " yuv=" + yuvCount
                    + " caps=" + Arrays.toString(caps));
        } catch (Throwable t) {
            log("CHARS error=" + t.getClass().getSimpleName() + ": " + t.getMessage());
        }
    }

    private void queryUnknown() {
        try {
            cameraManager.getCameraCharacteristics("99");
            log("UNKNOWN unexpected_ok");
        } catch (Throwable t) {
            log("UNKNOWN error=" + t.getClass().getSimpleName() + ": " + t.getMessage());
        }
    }

    private void openCamera() {
        try {
            cameraManager.openCamera("0", new CameraDevice.StateCallback() {
                @Override
                public void onOpened(CameraDevice camera) {
                    log("OPEN callback=opened id=" + camera.getId());
                    camera.close();
                }

                @Override
                public void onDisconnected(CameraDevice camera) {
                    log("OPEN callback=disconnected");
                }

                @Override
                public void onError(CameraDevice camera, int error) {
                    log("OPEN callback=error:" + error);
                }
            }, new Handler(Looper.getMainLooper()));
            log("OPEN submitted");
        } catch (Throwable t) {
            log("OPEN error=" + t.getClass().getSimpleName() + ": " + t.getMessage());
        }
    }
}
