package dev.acos.fixtures.cameraprobe;

import android.app.Activity;
import android.content.Context;
import android.content.pm.PackageManager;
import android.graphics.ImageFormat;
import android.graphics.Rect;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CameraMetadata;
import android.hardware.camera2.CaptureFailure;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.TotalCaptureResult;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.os.Bundle;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.Looper;
import android.util.Log;
import android.util.Size;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.zip.CRC32;

/**
 * The `hw.camera` proof fixture (CAMERA2 Slice 1 + Slice 2).
 *
 * <p>Exercises the real framework {@link CameraManager}, {@link CameraCharacteristics},
 * {@link StreamConfigurationMap}, {@link CameraDevice}, {@link CameraCaptureSession},
 * {@link CaptureRequest}, {@link TotalCaptureResult}, and {@link ImageReader}
 * ({@code YUV_420_888} 640x480): camera enumeration, characteristics parsing,
 * unknown-id error handling, {@code CAMERA} permission enforcement, the off-mode
 * behavior (empty list + structured refusal on open), single-shot capture with
 * deterministic {@code ACOS_PATTERN_COLORBARS_V1} pixel &amp; CRC32 verification,
 * repeating capture (3 frames with strictly increasing frame numbers &amp; timestamps),
 * and clean close ({@code onClosed}).
 */
public final class MainActivity extends Activity {
    private static final String TAG = "cameraprobe";
    private static final int CAPTURE_WIDTH = 640;
    private static final int CAPTURE_HEIGHT = 480;
    private static final int REPEAT_TARGET_FRAMES = 3;

    private final Handler mainHandler = new Handler(Looper.getMainLooper());
    private TextView status;
    private CameraManager cameraManager;
    private int captureSeq = 0;

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
        root.addView(button("Capture", v -> runSingleCapture()));
        root.addView(button("Repeat", v -> runRepeatingCapture()));

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
        if (Looper.myLooper() == Looper.getMainLooper()) {
            appendStatus(line);
        } else {
            mainHandler.post(() -> appendStatus(line));
        }
    }

    private void appendStatus(String line) {
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
                public void onClosed(CameraDevice camera) {
                    log("OPEN callback=closed id=" + camera.getId());
                }

                @Override
                public void onDisconnected(CameraDevice camera) {
                    log("OPEN callback=disconnected");
                }

                @Override
                public void onError(CameraDevice camera, int error) {
                    log("OPEN callback=error:" + error);
                }
            }, mainHandler);
            log("OPEN submitted");
        } catch (Throwable t) {
            log("OPEN error=" + t.getClass().getSimpleName() + ": " + t.getMessage());
        }
    }

    private static final class YuvInspection {
        final int width;
        final int height;
        final int format;
        final int planeCount;
        final long timestampNs;
        final long crc32;
        final String white;
        final String green;
        final String red;
        final String black;
        final String tag;
        final String ramp;
        final String tl;
        final String tr;
        final String bl;
        final String br;
        final String ctr;
        final int tagByte0;

        YuvInspection(
                int width,
                int height,
                int format,
                int planeCount,
                long timestampNs,
                long crc32,
                String white,
                String green,
                String red,
                String black,
                String tag,
                String ramp,
                String tl,
                String tr,
                String bl,
                String br,
                String ctr,
                int tagByte0) {
            this.width = width;
            this.height = height;
            this.format = format;
            this.planeCount = planeCount;
            this.timestampNs = timestampNs;
            this.crc32 = crc32;
            this.white = white;
            this.green = green;
            this.red = red;
            this.black = black;
            this.tag = tag;
            this.ramp = ramp;
            this.tl = tl;
            this.tr = tr;
            this.bl = bl;
            this.br = br;
            this.ctr = ctr;
            this.tagByte0 = tagByte0;
        }
    }

    private static int readPlaneByte(Image.Plane plane, int x, int y) {
        ByteBuffer buf = plane.getBuffer();
        int idx = y * plane.getRowStride() + x * plane.getPixelStride();
        return buf.get(idx) & 0xFF;
    }

    private static String sampleYuv(Image.Plane[] planes, int x, int y) {
        int yv = readPlaneByte(planes[0], x, y);
        int uv = readPlaneByte(planes[1], x / 2, y / 2);
        int vv = readPlaneByte(planes[2], x / 2, y / 2);
        return "(" + yv + "," + uv + "," + vv + ")";
    }

    private static YuvInspection inspectYuv420Image(Image image) {
        int w = image.getWidth();
        int h = image.getHeight();
        int fmt = image.getFormat();
        long ts = image.getTimestamp();
        Image.Plane[] planes = image.getPlanes();

        CRC32 crc = new CRC32();
        // Plane 0: Y (w x h)
        updateCrcFromPlane(crc, planes[0], w, h);
        // Plane 1: U (w/2 x h/2)
        updateCrcFromPlane(crc, planes[1], w / 2, h / 2);
        // Plane 2: V (w/2 x h/2)
        updateCrcFromPlane(crc, planes[2], w / 2, h / 2);

        String white = sampleYuv(planes, 40, 100);
        String green = sampleYuv(planes, 280, 100);
        String red = sampleYuv(planes, 440, 100);
        String black = sampleYuv(planes, 600, 100);
        int t0 = readPlaneByte(planes[0], 0, 0);
        int t1 = readPlaneByte(planes[0], 1, 0);
        int t2 = readPlaneByte(planes[0], 2, 0);
        int t3 = readPlaneByte(planes[0], 3, 0);
        String tag = "(" + t0 + "," + t1 + "," + t2 + "," + t3 + ")";
        String ramp = sampleYuv(planes, 320, 420);
        String tl = sampleYuv(planes, 160, 120);
        String tr = sampleYuv(planes, 480, 120);
        String bl = sampleYuv(planes, 160, 360);
        String br = sampleYuv(planes, 480, 360);
        String ctr = sampleYuv(planes, 320, 240);

        return new YuvInspection(
                w, h, fmt, planes.length, ts, crc.getValue(),
                white, green, red, black, tag, ramp, tl, tr, bl, br, ctr, t0);
    }

    private static void updateCrcFromPlane(CRC32 crc, Image.Plane plane, int width, int height) {
        ByteBuffer buf = plane.getBuffer();
        int rowStride = plane.getRowStride();
        int pixelStride = plane.getPixelStride();
        byte[] row = new byte[width];
        for (int y = 0; y < height; y++) {
            int rowBase = y * rowStride;
            if (pixelStride == 1) {
                buf.position(rowBase);
                buf.get(row, 0, width);
            } else {
                for (int x = 0; x < width; x++) {
                    row[x] = buf.get(rowBase + x * pixelStride);
                }
            }
            crc.update(row, 0, width);
        }
    }

    @SuppressWarnings("deprecation")
    private void runSingleCapture() {
        final HandlerThread bgThread = new HandlerThread("cameraprobe-capture");
        bgThread.start();
        final Handler bgHandler = new Handler(bgThread.getLooper());
        final ImageReader reader = ImageReader.newInstance(
                CAPTURE_WIDTH, CAPTURE_HEIGHT, ImageFormat.YUV_420_888, 4);

        final AtomicBoolean gotImage = new AtomicBoolean(false);
        final AtomicBoolean gotCompleted = new AtomicBoolean(false);
        final AtomicBoolean closing = new AtomicBoolean(false);
        final CameraDevice[] deviceRef = new CameraDevice[1];
        final CameraCaptureSession[] sessionRef = new CameraCaptureSession[1];

        final Runnable maybeClose = () -> {
            if (gotImage.get() && gotCompleted.get() && closing.compareAndSet(false, true)) {
                CameraCaptureSession s = sessionRef[0];
                if (s != null) {
                    try {
                        s.close();
                    } catch (Throwable ignored) {
                    }
                }
                CameraDevice d = deviceRef[0];
                if (d != null) {
                    d.close();
                }
            }
        };

        reader.setOnImageAvailableListener(r -> {
            Image img = null;
            try {
                img = r.acquireNextImage();
                if (img == null) {
                    return;
                }
                YuvInspection info = inspectYuv420Image(img);
                log(String.format(
                        Locale.US,
                        "CAPTURE image w=%d h=%d fmt=%d planes=%d ts=%d crc32=0x%08x white=%s green=%s red=%s black=%s tag=%s ramp=%s tl=%s tr=%s bl=%s br=%s ctr=%s",
                        info.width, info.height, info.format, info.planeCount,
                        info.timestampNs, info.crc32,
                        info.white, info.green, info.red, info.black, info.tag, info.ramp,
                        info.tl, info.tr, info.bl, info.br, info.ctr));
                gotImage.set(true);
                maybeClose.run();
            } catch (Throwable t) {
                log("CAPTURE image_error=" + t.getClass().getSimpleName() + ": " + t.getMessage());
            } finally {
                if (img != null) {
                    img.close();
                }
            }
        }, bgHandler);

        try {
            cameraManager.openCamera("0", new CameraDevice.StateCallback() {
                @Override
                public void onOpened(CameraDevice camera) {
                    deviceRef[0] = camera;
                    log("CAPTURE opened id=" + camera.getId());
                    try {
                        camera.createCaptureSession(
                                Collections.singletonList(reader.getSurface()),
                                new CameraCaptureSession.StateCallback() {
                                    @Override
                                    public void onConfigured(CameraCaptureSession session) {
                                        sessionRef[0] = session;
                                        try {
                                            CaptureRequest.Builder req = camera.createCaptureRequest(
                                                    CameraDevice.TEMPLATE_STILL_CAPTURE);
                                            req.addTarget(reader.getSurface());
                                            session.capture(
                                                    req.build(),
                                                    new CameraCaptureSession.CaptureCallback() {
                                                        @Override
                                                        public void onCaptureCompleted(
                                                                CameraCaptureSession s,
                                                                CaptureRequest request,
                                                                TotalCaptureResult result) {
                                                            Long ts = result.get(
                                                                    TotalCaptureResult.SENSOR_TIMESTAMP);
                                                            log("CAPTURE completed frame="
                                                                    + result.getFrameNumber()
                                                                    + " ts=" + ts);
                                                            gotCompleted.set(true);
                                                            maybeClose.run();
                                                        }

                                                        @Override
                                                        public void onCaptureFailed(
                                                                CameraCaptureSession s,
                                                                CaptureRequest request,
                                                                CaptureFailure failure) {
                                                            log("CAPTURE failed reason="
                                                                    + failure.getReason());
                                                            camera.close();
                                                        }
                                                    },
                                                    bgHandler);
                                        } catch (Throwable t) {
                                            log("CAPTURE submit_error="
                                                    + t.getClass().getSimpleName() + ": "
                                                    + t.getMessage());
                                            camera.close();
                                        }
                                    }

                                    @Override
                                    public void onConfigureFailed(CameraCaptureSession session) {
                                        log("CAPTURE configure_failed");
                                        camera.close();
                                    }
                                },
                                bgHandler);
                    } catch (Throwable t) {
                        log("CAPTURE session_error=" + t.getClass().getSimpleName() + ": " + t.getMessage());
                        camera.close();
                    }
                }

                @Override
                public void onClosed(CameraDevice camera) {
                    reader.close();
                    bgThread.quitSafely();
                    captureSeq++;
                    log("CAPTURE closed id=" + camera.getId() + " seq=" + captureSeq);
                }

                @Override
                public void onDisconnected(CameraDevice camera) {
                    log("CAPTURE disconnected");
                    camera.close();
                }

                @Override
                public void onError(CameraDevice camera, int error) {
                    log("CAPTURE device_error=" + error);
                    camera.close();
                }
            }, bgHandler);
        } catch (Throwable t) {
            reader.close();
            bgThread.quitSafely();
            log("CAPTURE error=" + t.getClass().getSimpleName() + ": " + t.getMessage());
        }
    }

    @SuppressWarnings("deprecation")
    private void runRepeatingCapture() {
        final HandlerThread bgThread = new HandlerThread("cameraprobe-repeat");
        bgThread.start();
        final Handler bgHandler = new Handler(bgThread.getLooper());
        final ImageReader reader = ImageReader.newInstance(
                CAPTURE_WIDTH, CAPTURE_HEIGHT, ImageFormat.YUV_420_888, 6);

        final List<Long> completedFrames = new ArrayList<>();
        final List<Long> completedTimestamps = new ArrayList<>();
        final List<String> imageCrcs = new ArrayList<>();
        final List<Long> imageTimestamps = new ArrayList<>();
        final List<Integer> imageTags = new ArrayList<>();
        final AtomicBoolean stopping = new AtomicBoolean(false);
        final CameraDevice[] deviceRef = new CameraDevice[1];
        final CameraCaptureSession[] sessionRef = new CameraCaptureSession[1];

        final Runnable maybeFinish = () -> {
            if (completedFrames.size() >= REPEAT_TARGET_FRAMES
                    && imageCrcs.size() >= REPEAT_TARGET_FRAMES
                    && stopping.compareAndSet(false, true)) {
                boolean framesInc = true;
                boolean tsInc = true;
                boolean imgTsInc = true;
                for (int i = 1; i < REPEAT_TARGET_FRAMES; i++) {
                    if (completedFrames.get(i) <= completedFrames.get(i - 1)) {
                        framesInc = false;
                    }
                    if (completedTimestamps.get(i) <= completedTimestamps.get(i - 1)) {
                        tsInc = false;
                    }
                    if (imageTimestamps.get(i) <= imageTimestamps.get(i - 1)) {
                        imgTsInc = false;
                    }
                }
                log("REPEAT ok count=" + REPEAT_TARGET_FRAMES
                        + " frames=" + completedFrames.subList(0, REPEAT_TARGET_FRAMES)
                        + " frames_inc=" + framesInc
                        + " ts_inc=" + tsInc
                        + " img_ts_inc=" + imgTsInc
                        + " crcs=" + imageCrcs.subList(0, REPEAT_TARGET_FRAMES)
                        + " tags=" + imageTags.subList(0, REPEAT_TARGET_FRAMES));
                CameraCaptureSession s = sessionRef[0];
                if (s != null) {
                    try {
                        s.stopRepeating();
                    } catch (Throwable ignored) {
                    }
                    try {
                        s.close();
                    } catch (Throwable ignored) {
                    }
                }
                CameraDevice d = deviceRef[0];
                if (d != null) {
                    d.close();
                }
            }
        };

        reader.setOnImageAvailableListener(r -> {
            Image img = null;
            try {
                img = r.acquireNextImage();
                if (img == null) {
                    return;
                }
                if (imageCrcs.size() < REPEAT_TARGET_FRAMES) {
                    YuvInspection info = inspectYuv420Image(img);
                    imageCrcs.add(String.format(Locale.US, "0x%08x", info.crc32));
                    imageTimestamps.add(info.timestampNs);
                    imageTags.add(info.tagByte0);
                    maybeFinish.run();
                }
            } catch (Throwable t) {
                log("REPEAT image_error=" + t.getClass().getSimpleName() + ": " + t.getMessage());
            } finally {
                if (img != null) {
                    img.close();
                }
            }
        }, bgHandler);

        try {
            cameraManager.openCamera("0", new CameraDevice.StateCallback() {
                @Override
                public void onOpened(CameraDevice camera) {
                    deviceRef[0] = camera;
                    try {
                        camera.createCaptureSession(
                                Collections.singletonList(reader.getSurface()),
                                new CameraCaptureSession.StateCallback() {
                                    @Override
                                    public void onConfigured(CameraCaptureSession session) {
                                        sessionRef[0] = session;
                                        try {
                                            CaptureRequest.Builder req = camera.createCaptureRequest(
                                                    CameraDevice.TEMPLATE_PREVIEW);
                                            req.addTarget(reader.getSurface());
                                            session.setRepeatingRequest(
                                                    req.build(),
                                                    new CameraCaptureSession.CaptureCallback() {
                                                        @Override
                                                        public void onCaptureCompleted(
                                                                CameraCaptureSession s,
                                                                CaptureRequest request,
                                                                TotalCaptureResult result) {
                                                            if (completedFrames.size()
                                                                    < REPEAT_TARGET_FRAMES) {
                                                                Long ts = result.get(
                                                                        TotalCaptureResult.SENSOR_TIMESTAMP);
                                                                completedFrames.add(
                                                                        result.getFrameNumber());
                                                                completedTimestamps.add(
                                                                        ts == null ? 0L : ts);
                                                                maybeFinish.run();
                                                            }
                                                        }
                                                    },
                                                    bgHandler);
                                        } catch (Throwable t) {
                                            log("REPEAT submit_error="
                                                    + t.getClass().getSimpleName() + ": "
                                                    + t.getMessage());
                                            camera.close();
                                        }
                                    }

                                    @Override
                                    public void onConfigureFailed(CameraCaptureSession session) {
                                        log("REPEAT configure_failed");
                                        camera.close();
                                    }
                                },
                                bgHandler);
                    } catch (Throwable t) {
                        log("REPEAT session_error=" + t.getClass().getSimpleName() + ": " + t.getMessage());
                        camera.close();
                    }
                }

                @Override
                public void onClosed(CameraDevice camera) {
                    reader.close();
                    bgThread.quitSafely();
                    log("REPEAT closed id=" + camera.getId());
                }

                @Override
                public void onDisconnected(CameraDevice camera) {
                    log("REPEAT disconnected");
                    camera.close();
                }

                @Override
                public void onError(CameraDevice camera, int error) {
                    log("REPEAT device_error=" + error);
                    camera.close();
                }
            }, bgHandler);
        } catch (Throwable t) {
            reader.close();
            bgThread.quitSafely();
            log("REPEAT error=" + t.getClass().getSimpleName() + ": " + t.getMessage());
        }
    }
}
