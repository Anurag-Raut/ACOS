package dev.acos.fixtures.settletab;

import android.app.Activity;
import android.os.Bundle;
import android.view.View;

/** Target of settle-lab's Activity transition case. */
public class SecondActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.second);
        findViewById(R.id.second_finish).setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View v) { finish(); }
        });
    }
}
