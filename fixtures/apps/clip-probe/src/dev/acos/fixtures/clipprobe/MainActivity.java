package dev.acos.fixtures.clipprobe;

import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.pm.PackageManager;
import android.os.Bundle;
import android.os.Looper;
import android.os.SystemClock;
import android.util.Log;
import android.view.Gravity;
import android.view.View;
import android.view.autofill.AutofillManager;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

/**
 * The `core` clipboard + autofill proof fixture (MODFILL).
 *
 * <p>Both services are `core` (always attached), so this app talks to the real
 * {@code IClipboard} and the real {@code IAutoFillManager} through the
 * framework's own client classes. Every line it prints is a value the
 * framework answered or an event it delivered:
 *
 * <ul>
 *   <li>clipboard: {@code setPrimaryClip(ClipData)} → {@code getPrimaryClip()}
 *       (the {@code ClipData} parcel round trip) and
 *       {@code addPrimaryClipChangedListener} → {@code onPrimaryClipChanged()}
 *       (a oneway binder callback from the service back into the app);</li>
 *   <li>autofill: {@code AutofillManager.isEnabled()} /
 *       {@code hasEnabledAutofillServices()} (both decided by the service's
 *       {@code addClient} reply flags, i.e. a synchronous binder round trip),
 *       {@code requestAutofill(View)} and the resulting field text (nothing
 *       fills when no autofill service exists — the honest answer, printed as
 *       the field's value, never as a claim).</li>
 * </ul>
 *
 * <p>Lines are appended, never replaced, so a scenario's {@code wait_for} can
 * wait for a specific line after it taps the button that produces it.
 */
public final class MainActivity extends Activity {
    private static final String LOG_TAG = "clipprobe";

    private LinearLayout root;
    private TextView status;
    private TextView title;
    private EditText field;
    private ClipboardManager clipboard;
    private ClipboardManager.OnPrimaryClipChangedListener listener;
    private int clipChanged;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(24, 24, 24, 24);

        title = new TextView(this);
        title.setId(R.id.title);
        title.setTextSize(20);
        title.setText("CLIPPROBE");
        root.addView(title);

        root.addView(button("Clipboard", "clipboard-tab", view -> {
            title.setText("CLIPBOARD");
            log("TAB clipboard");
        }));
        root.addView(button("Autofill", "autofill-tab", view -> {
            title.setText("AUTOFILL");
            log("TAB autofill");
        }));

        status = new TextView(this);
        status.setId(R.id.status);
        status.setTextIsSelectable(true);

        clipboard = getSystemService(ClipboardManager.class);
        showClipboard();
        showAutofill();
        // The log goes last: it grows, and a control below it would be pushed
        // off the screen.
        root.addView(status);

        // Like the other probes: the log grows, so it lives in a ScrollView
        // (a bare LinearLayout squeezes the newest line to zero height).
        ScrollView scroll = new ScrollView(this);
        scroll.addView(root);
        setContentView(scroll);

        title.setText("CLIPPROBE");
        log("READY clipboard=" + (clipboard == null ? "none" : "present")
                + " autofillFeature="
                + getPackageManager().hasSystemFeature(PackageManager.FEATURE_AUTOFILL));
    }

    private Button button(String label, String tag, View.OnClickListener click) {
        Button button = new Button(this);
        button.setTag(tag);
        button.setText(label);
        button.setGravity(Gravity.START);
        button.setOnClickListener(click);
        return button;
    }

    private void log(String line) {
        // Thread-safe evidence channel first (`Log.i` reaches `acos_logs`), the
        // TextView second: an `onPrimaryClipChanged()` callback runs on a binder
        // thread, so appending directly from it would race the UI thread's
        // `Editable` and silently drop lines.
        Log.i(LOG_TAG, line);
        Runnable append = () -> status.append(line + "\n");
        if (Looper.myLooper() == Looper.getMainLooper()) {
            append.run();
        } else {
            status.post(append);
        }
    }

    // ------------------------------------------------------------ clipboard

    private void showClipboard() {
        if (clipboard == null) {
            log("CLIPBOARD none (ClipboardManager is null)");
            return;
        }
        root.addView(button("Listen", "clip-listen", view -> registerListener()));
        root.addView(button("Copy", "clip-copy", view -> copy("proof-one")));
        root.addView(button("Copy2", "clip-copy2", view -> copy("proof-two")));
        root.addView(button("Paste", "clip-paste", view -> paste()));
        root.addView(button("Clear", "clip-clear", view -> {
            try {
                clipboard.clearPrimaryClip();
                log("CLEARED hasPrimaryClip=" + clipboard.hasPrimaryClip());
            } catch (Throwable failure) {
                log("CLEAR threw=" + failure.getClass().getName() + ": " + failure.getMessage());
            }
        }));
        root.addView(button("Query", "clip-query", view -> {
            try {
                log("QUERY hasPrimaryClip=" + clipboard.hasPrimaryClip());
            } catch (Throwable failure) {
                log("QUERY threw=" + failure.getClass().getName() + ": " + failure.getMessage());
            }
        }));
        root.addView(button("Describe", "clip-describe", view -> {
            ClipData clip = clipboard.getPrimaryClip();
            if (clip == null) {
                log("DESCRIBE none");
                return;
            }
            CharSequence label = clip.getDescription().getLabel();
            int mimes = clip.getDescription().getMimeTypeCount();
            log("DESCRIBE label=" + (label == null ? "null" : label)
                    + " items=" + clip.getItemCount()
                    + " mimes=" + mimes);
        }));
    }

    private void registerListener() {
        if (listener != null) {
            log("LISTENER already-registered count=" + clipChanged);
            return;
        }
        listener = () -> {
            int count;
            String text;
            synchronized (MainActivity.this) {
                count = ++clipChanged;
            }
            // The listener is called with no arguments (API 33+ has a variant
            // that reports whether the change came from the app itself), so the
            // clipboard is read again — exactly what a real app does.
            ClipData clip = clipboard.getPrimaryClip();
            text = firstText(clip);
            log("CLIPCHANGED n=" + count + " text=" + text);
        };
        clipboard.addPrimaryClipChangedListener(listener);
        log("LISTENER registered");
    }

    private void copy(String text) {
        try {
            clipboard.setPrimaryClip(ClipData.newPlainText("acos", text));
            log("COPY ok text=" + text + " hasPrimaryClip=" + clipboard.hasPrimaryClip());
        } catch (Throwable failure) {
            log("COPY threw=" + failure.getClass().getName() + ": " + failure.getMessage());
        }
    }

    private void paste() {
        try {
            ClipData clip = clipboard.getPrimaryClip();
            if (clip == null) {
                log("PASTED none items=0");
                return;
            }
            log("PASTED items=" + clip.getItemCount() + " text=" + firstText(clip));
        } catch (Throwable failure) {
            log("PASTE threw=" + failure.getClass().getName() + ": " + failure.getMessage());
        }
    }

    private static String firstText(ClipData clip) {
        if (clip == null || clip.getItemCount() == 0) {
            return "none";
        }
        CharSequence text = clip.getItemAt(0).getText();
        return text == null ? "null" : text.toString();
    }

    // ------------------------------------------------------------- autofill

    private void showAutofill() {
        field = new EditText(this);
        field.setId(R.id.field);
        field.setHint("autofill target");
        field.setSingleLine(true);
        root.addView(field);
        // "Request", not a second "Autofill": a scenario taps by text and would
        // hit the tab button instead (a duplicate label cost this probe a run).
        root.addView(button("Request", "autofill-run", view -> autofill()));
        root.addView(button("ReadField", "autofill-read", view -> readField()));
    }

    private void autofill() {
        PackageManager pm = getPackageManager();
        log("AUTOFILL feature=" + pm.hasSystemFeature(PackageManager.FEATURE_AUTOFILL));
        AutofillManager manager;
        try {
            manager = getSystemService(AutofillManager.class);
        } catch (Throwable failure) {
            log("AUTOFILL manager-threw=" + failure.getClass().getName() + ": "
                    + failure.getMessage());
            return;
        }
        if (manager == null) {
            log("AUTOFILL manager=none");
            return;
        }
        log("AUTOFILL manager=present");
        // `isEnabled()` is the app-visible result of the service's `addClient`
        // reply flags (a synchronous IResultReceiver round trip): false means
        // no autofill service exists, which is ACOS's honest state.
        // `isEnabled()` is the app-visible result of the service's `addClient`
        // reply (a *synchronous* IResultReceiver round trip with a 15 s client
        // timeout), so the elapsed time is evidence: a millisecond answer is the
        // host's reply, a 15000 ms one is the framework giving up.
        try {
            long started = SystemClock.uptimeMillis();
            boolean enabled = manager.isEnabled();
            long took = SystemClock.uptimeMillis() - started;
            log("AUTOFILL enabled=" + enabled + " took_ms=" + took);
        } catch (Throwable failure) {
            log("AUTOFILL enabled-threw=" + failure.getClass().getName() + ": "
                    + failure.getMessage());
        }
        try {
            log("AUTOFILL services=" + manager.hasEnabledAutofillServices());
        } catch (Throwable failure) {
            log("AUTOFILL services-threw=" + failure.getClass().getName() + ": "
                    + failure.getMessage());
        }
        // The explicit request (`View.requestAutofill` → `AutofillManager.
        // requestAutofill`) the user's "Autofill" menu entry performs.
        field.requestFocus();
        if (field.getText().length() == 0) {
            field.setText("typed-by-app");
        }
        try {
            manager.requestAutofill(field);
            log("AUTOFILL request=ok");
        } catch (Throwable failure) {
            log("AUTOFILL request-threw=" + failure.getClass().getName() + ": "
                    + failure.getMessage());
        }
    }

    private void readField() {
        log("FIELD text=" + field.getText());
    }
}
