package dev.acos.fixtures.widgetgallery;

import android.app.Activity;
import android.graphics.Typeface;
import android.os.Bundle;
import android.text.SpannableString;
import android.text.Spanned;
import android.text.style.StyleSpan;
import android.util.Log;
import android.widget.TextView;

/**
 * Widget-gallery fixture: framework widgets agents commonly use — Button states,
 * CheckBox, Switch, RadioButton, determinate ProgressBar, EditText hint,
 * ellipsized text, RTL text and a multi-style spanned run. Plain framework APIs.
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosWidgets";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);

        TextView styles = (TextView) findViewById(R.id.styles);
        SpannableString span = new SpannableString("bold italic normal");
        span.setSpan(new StyleSpan(Typeface.BOLD), 0, 4, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
        span.setSpan(new StyleSpan(Typeface.ITALIC), 5, 11, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
        styles.setText(span);
    }
}
