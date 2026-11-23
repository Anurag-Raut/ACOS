package dev.acos.fixtures.cameraxprobe;

import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Color;
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

import androidx.annotation.NonNull;
import androidx.appcompat.app.AppCompatActivity;
import androidx.camera.core.Camera;
import androidx.camera.core.CameraSelector;
import androidx.camera.core.ImageAnalysis;
import androidx.camera.core.ImageCapture;
import androidx.camera.core.ImageCaptureException;
import androidx.camera.core.ImageProxy;
import androidx.camera.core.Preview;
import androidx.camera.core.resolutionselector.ResolutionSelector;
import androidx.camera.core.resolutionselector.ResolutionStrategy;
import androidx.camera.lifecycle.ProcessCameraProvider;
import androidx.camera.view.PreviewView;
import androidx.core.content.ContextCompat;

import com.google.common.util.concurrent.ListenableFuture;

import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.zip.CRC32;

public class MainActivity extends AppCompatActivity {
    private static final String TAG = "cameraxprobe";
    private static final int TARGET_WIDTH = 640;
    private static final int TARGET_HEIGHT = 480;
    private static final int REQUIRED_ANALYSIS_FRAMES = 3;

    private final Handler mainHandler = new Handler(Looper.getMainLooper());
    private final ExecutorService cameraExecutor = Executors.newSingleThreadExecutor();

    private PreviewView previewView;
    private TextView status;

    private ProcessCameraProvider cameraProvider;
    private ImageAnalysis imageAnalysis;
    private ImageCapture imageCapture;
    private Preview previewUseCase;

    private final AtomicInteger analysisCount = new AtomicInteger(0);
    private final List<Long> analysisTimestamps = new ArrayList<>();
    private final List<String> analysisCrcs = new ArrayList<>();
    private final List<Integer> analysisTags = new ArrayList<>();
    private boolean analysisSummaryLogged = false;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(24, 24, 24, 24);

        TextView title = new TextView(this);
        title.setId(R.id.title);
        title.setText("CameraXProbe");
        title.setTextSize(20f);
        root.addView(title);

        previewView = new PreviewView(this);
        previewView.setId(R.id.preview_view);
        root.addView(previewView, new LinearLayout.LayoutParams(TARGET_WIDTH, TARGET_HEIGHT));

        root.addView(buttonRow(
                button("Bind", v -> bindAnalysisAndCapture(false)),
                button("Bind Preview", v -> bindAnalysisAndCapture(true))));
        root.addView(buttonRow(
                button("Capture", v -> runImageCapture()),
                button("Unbind", v -> unbindAll())));

        status = new TextView(this);
        status.setId(R.id.status);
        status.setTextIsSelectable(true);

        ScrollView scroll = new ScrollView(this);
        scroll.addView(status);
        root.addView(scroll, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, 0, 1.0f));
        setContentView(root);

        previewView.getPreviewStreamState().observe(this, state -> {
            if (state != null) {
                log("PREVIEW_STATE state=" + state.name()
                        + " impl=" + previewView.getImplementationMode().name());
            }
        });

        PackageManager pm = getPackageManager();
        boolean featCam = pm.hasSystemFeature(PackageManager.FEATURE_CAMERA);
        boolean featAny = pm.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY);
        log("READY feat_cam=" + featCam + " feat_any=" + featAny);
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        cameraExecutor.shutdown();
    }

    private LinearLayout buttonRow(Button left, Button right) {
        LinearLayout row = new LinearLayout(this);
        row.setOrientation(LinearLayout.HORIZONTAL);
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(
                0, LinearLayout.LayoutParams.WRAP_CONTENT, 1.0f);
        row.addView(left, lp);
        row.addView(right, new LinearLayout.LayoutParams(
                0, LinearLayout.LayoutParams.WRAP_CONTENT, 1.0f));
        return row;
    }

    private Button button(String label, View.OnClickListener onClick) {
        Button b = new Button(this);
        b.setAllCaps(false);
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

    private static String rootCauseString(Throwable t) {
        StringBuilder sb = new StringBuilder();
        Throwable cur = t;
        int depth = 0;
        while (cur != null && depth < 6) {
            if (depth > 0) sb.append(" <- ");
            sb.append(cur.getClass().getSimpleName()).append(": ").append(cur.getMessage());
            cur = cur.getCause();
            depth++;
        }
        return sb.toString();
    }

    private void bindAnalysisAndCapture(boolean withPreview) {
        try {
            ListenableFuture<ProcessCameraProvider> future =
                    ProcessCameraProvider.getInstance(this);
            future.addListener(() -> {
                try {
                    cameraProvider = future.get();
                    int camCount = cameraProvider.getAvailableCameraInfos().size();
                    boolean hasBack = cameraProvider.hasCamera(CameraSelector.DEFAULT_BACK_CAMERA);
                    log("PROVIDER ok cameras=" + camCount + " has_back=" + hasBack);

                    cameraProvider.unbindAll();
                    synchronized (analysisTimestamps) {
                        analysisCount.set(0);
                        analysisTimestamps.clear();
                        analysisCrcs.clear();
                        analysisTags.clear();
                        analysisSummaryLogged = false;
                    }

                    ResolutionSelector rs = new ResolutionSelector.Builder()
                            .setResolutionStrategy(new ResolutionStrategy(
                                    new Size(TARGET_WIDTH, TARGET_HEIGHT),
                                    ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER))
                            .build();

                    imageAnalysis = new ImageAnalysis.Builder()
                            .setResolutionSelector(rs)
                            .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                            .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888)
                            .build();

                    imageAnalysis.setAnalyzer(cameraExecutor, this::onAnalysisImage);

                    imageCapture = new ImageCapture.Builder()
                            .setResolutionSelector(rs)
                            .build();

                    Camera camera;
                    if (withPreview) {
                        previewUseCase = new Preview.Builder()
                                .setResolutionSelector(rs)
                                .build();
                        previewUseCase.setSurfaceProvider(previewView.getSurfaceProvider());
                        camera = cameraProvider.bindToLifecycle(
                                this,
                                CameraSelector.DEFAULT_BACK_CAMERA,
                                previewUseCase,
                                imageAnalysis,
                                imageCapture);
                        log("BIND_PREVIEW ok impl=" + previewView.getImplementationMode().name()
                                + " cam=" + camera.getCameraInfo().getSensorRotationDegrees());
                    } else {
                        camera = cameraProvider.bindToLifecycle(
                                this,
                                CameraSelector.DEFAULT_BACK_CAMERA,
                                imageAnalysis,
                                imageCapture);
                        log("BIND ok rot=" + camera.getCameraInfo().getSensorRotationDegrees());
                    }
                } catch (Throwable t) {
                    Log.e(TAG, "BIND failed", t);
                    log("BIND error=" + rootCauseString(t));
                }
            }, ContextCompat.getMainExecutor(this));
        } catch (Throwable t) {
            Log.e(TAG, "PROVIDER failed", t);
            log("PROVIDER error=" + rootCauseString(t));
        }
    }

    private void onAnalysisImage(@NonNull ImageProxy image) {
        try {
            int n = analysisCount.incrementAndGet();
            int w = image.getWidth();
            int h = image.getHeight();
            int fmt = image.getFormat();
            long ts = image.getImageInfo().getTimestamp();
            int rot = image.getImageInfo().getRotationDegrees();
            ImageProxy.PlaneProxy[] planes = image.getPlanes();

            CRC32 crc = new CRC32();
            updateCrcFromPlane(crc, planes[0], w, h);
            updateCrcFromPlane(crc, planes[1], w / 2, h / 2);
            updateCrcFromPlane(crc, planes[2], w / 2, h / 2);
            String crcHex = String.format(Locale.US, "0x%08x", crc.getValue());

            int t0 = readPlaneByte(planes[0], 0, 0);
            int t1 = readPlaneByte(planes[0], 1, 0);
            int t2 = readPlaneByte(planes[0], 2, 0);
            int t3 = readPlaneByte(planes[0], 3, 0);
            String tag = "(" + t0 + "," + t1 + "," + t2 + "," + t3 + ")";

            if (n == 1) {
                String white = sampleYuv(planes, 40 * w / 640, 100 * h / 480);
                String green = sampleYuv(planes, 280 * w / 640, 100 * h / 480);
                String red = sampleYuv(planes, 440 * w / 640, 100 * h / 480);
                String black = sampleYuv(planes, 600 * w / 640, 100 * h / 480);
                String ramp = sampleYuv(planes, 320 * w / 640, 420 * h / 480);
                log(String.format(
                        Locale.US,
                        "ANALYSIS frame=1 w=%d h=%d fmt=%d planes=%d rot=%d ts=%d crc32=%s white=%s green=%s red=%s black=%s tag=%s ramp=%s",
                        w, h, fmt, planes.length, rot, ts, crcHex, white, green, red, black, tag, ramp));
            }

            synchronized (analysisTimestamps) {
                if (analysisTimestamps.size() < REQUIRED_ANALYSIS_FRAMES) {
                    analysisTimestamps.add(ts);
                    analysisCrcs.add(crcHex);
                    analysisTags.add(t0);
                }
                if (!analysisSummaryLogged && analysisTimestamps.size() >= REQUIRED_ANALYSIS_FRAMES) {
                    analysisSummaryLogged = true;
                    boolean tsInc = true;
                    for (int i = 1; i < analysisTimestamps.size(); i++) {
                        if (analysisTimestamps.get(i) <= analysisTimestamps.get(i - 1)) {
                            tsInc = false;
                        }
                    }
                    boolean tagsInc = true;
                    for (int i = 1; i < analysisTags.size(); i++) {
                        if (analysisTags.get(i) <= analysisTags.get(i - 1)) {
                            tagsInc = false;
                        }
                    }
                    log("ANALYSIS ok count=" + analysisTimestamps.size()
                            + " total=" + n
                            + " frames_inc=" + tagsInc
                            + " ts_inc=" + tsInc
                            + " crcs=" + analysisCrcs
                            + " tags=" + analysisTags);
                }
            }
        } catch (Throwable t) {
            Log.e(TAG, "ANALYSIS failed", t);
            log("ANALYSIS error=" + rootCauseString(t));
        } finally {
            image.close();
        }
    }

    private void runImageCapture() {
        if (imageCapture == null) {
            log("CAPTURE error=not_bound");
            return;
        }
        try {
            imageCapture.takePicture(cameraExecutor, new ImageCapture.OnImageCapturedCallback() {
                @Override
                public void onCaptureSuccess(@NonNull ImageProxy image) {
                    try {
                        int w = image.getWidth();
                        int h = image.getHeight();
                        int fmt = image.getFormat();
                        long ts = image.getImageInfo().getTimestamp();
                        ByteBuffer buf = image.getPlanes()[0].getBuffer();
                        byte[] jpegBytes = new byte[buf.remaining()];
                        buf.get(jpegBytes);
                        boolean soi = jpegBytes.length >= 2
                                && (jpegBytes[0] & 0xFF) == 0xFF
                                && (jpegBytes[1] & 0xFF) == 0xD8;
                        Bitmap bmp = BitmapFactory.decodeByteArray(jpegBytes, 0, jpegBytes.length);
                        if (bmp == null) {
                            log("CAPTURE decode_error bytes=" + jpegBytes.length + " soi=" + soi);
                        } else {
                            int bw = bmp.getWidth();
                            int bh = bmp.getHeight();
                            int pWhite = bmp.getPixel(40 * bw / 640, 100 * bh / 480);
                            int pGreen = bmp.getPixel(280 * bw / 640, 100 * bh / 480);
                            int pRed = bmp.getPixel(440 * bw / 640, 100 * bh / 480);
                            int pBlack = bmp.getPixel(600 * bw / 640, 100 * bh / 480);
                            int err = 0;
                            err = Math.max(err, channelMaxDiff(pWhite, 255, 255, 255));
                            err = Math.max(err, channelMaxDiff(pGreen, 0, 255, 0));
                            err = Math.max(err, channelMaxDiff(pRed, 255, 0, 0));
                            err = Math.max(err, channelMaxDiff(pBlack, 0, 0, 0));
                            log(String.format(
                                    Locale.US,
                                    "CAPTURE image w=%d h=%d fmt=%d ts=%d bytes=%d soi=%s bmp=%dx%d white=%s green=%s red=%s black=%s max_err=%d tol4=%s",
                                    w, h, fmt, ts, jpegBytes.length, soi, bw, bh,
                                    formatRgb(pWhite), formatRgb(pGreen), formatRgb(pRed), formatRgb(pBlack),
                                    err, (err <= 4)));
                            bmp.recycle();
                        }
                    } catch (Throwable t) {
                        Log.e(TAG, "CAPTURE decode failed", t);
                        log("CAPTURE error=" + rootCauseString(t));
                    } finally {
                        image.close();
                    }
                }

                @Override
                public void onError(@NonNull ImageCaptureException exception) {
                    Log.e(TAG, "CAPTURE failed code=" + exception.getImageCaptureError(), exception);
                    log("CAPTURE error=ImageCaptureException(" + exception.getImageCaptureError()
                            + "): " + rootCauseString(exception));
                }
            });
        } catch (Throwable t) {
            Log.e(TAG, "CAPTURE throw", t);
            log("CAPTURE error=" + rootCauseString(t));
        }
    }

    private void unbindAll() {
        try {
            if (cameraProvider != null) {
                cameraProvider.unbindAll();
            }
            log("UNBIND ok");
        } catch (Throwable t) {
            Log.e(TAG, "UNBIND failed", t);
            log("UNBIND error=" + rootCauseString(t));
        }
    }

    private static int readPlaneByte(ImageProxy.PlaneProxy plane, int x, int y) {
        ByteBuffer buf = plane.getBuffer();
        int idx = y * plane.getRowStride() + x * plane.getPixelStride();
        return buf.get(idx) & 0xFF;
    }

    private static String sampleYuv(ImageProxy.PlaneProxy[] planes, int x, int y) {
        int yv = readPlaneByte(planes[0], x, y);
        int uv = readPlaneByte(planes[1], x / 2, y / 2);
        int vv = readPlaneByte(planes[2], x / 2, y / 2);
        return "(" + yv + "," + uv + "," + vv + ")";
    }

    private static void updateCrcFromPlane(CRC32 crc, ImageProxy.PlaneProxy plane, int width, int height) {
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

    private static String formatRgb(int px) {
        return "(" + Color.red(px) + "," + Color.green(px) + "," + Color.blue(px) + ")";
    }

    private static int channelMaxDiff(int px, int er, int eg, int eb) {
        int dr = Math.abs(Color.red(px) - er);
        int dg = Math.abs(Color.green(px) - eg);
        int db = Math.abs(Color.blue(px) - eb);
        return Math.max(dr, Math.max(dg, db));
    }
}
