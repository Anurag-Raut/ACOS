package dev.acos.fixtures.taptargets;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.widget.Button;
import android.widget.CompoundButton;
import android.widget.Switch;
import android.widget.TextView;

/**
 * REL3E fixture: one non-clickable label beside a clickable switch, and one
 * clickable button that changes state.
 *
 * Observable behaviour:
 *  - TextView id=title carries the text "Floating button" and is <b>not</b>
 *    clickable; no ancestor of it is clickable either. A tap on it is a press
 *    the app never consumes, so the window acknowledges it
 *    {@code finishInputEvent(handled) = false} and nothing changes.
 *  - Switch id=toggle is the clickable control in the same row and toggles
 *    TextView id=state between "off" and "on".
 *  - Button id=inc increments TextView id=count.
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosTapTargets";

    private TextView stateView;
    private TextView countView;
    private int count = 0;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        stateView = (TextView) findViewById(R.id.state);
        countView = (TextView) findViewById(R.id.count);
        ((Switch) findViewById(R.id.toggle))
                .setOnCheckedChangeListener(new CompoundButton.OnCheckedChangeListener() {
                    @Override
                    public void onCheckedChanged(CompoundButton button, boolean checked) {
                        stateView.setText(checked ? R.string.state_on : R.string.state_off);
                        Log.i(TAG, "toggle checked=" + checked);
                    }
                });
        ((Button) findViewById(R.id.inc)).setOnClickListener(new android.view.View.OnClickListener() {
            @Override
            public void onClick(android.view.View v) {
                count++;
                countView.setText(String.valueOf(count));
                Log.i(TAG, "increment value=" + count);
            }
        });
    }
}
