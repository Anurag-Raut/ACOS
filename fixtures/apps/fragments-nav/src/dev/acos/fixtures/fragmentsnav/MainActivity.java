package dev.acos.fixtures.fragmentsnav;

import android.app.Activity;
import android.app.FragmentTransaction;
import android.content.Intent;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

/**
 * Fragment-navigation fixture, launcher activity. Plain framework APIs only
 * ({@code android.app.Fragment}, {@code FragmentManager},
 * {@code FragmentTransaction}) with no AndroidX.
 *
 * Observable behaviour:
 *  - Show Alpha / Show Beta replace the container fragment and push the
 *    transaction onto the back stack
 *  - Back stack pops the previous fragment ({@code popBackStack})
 *  - Second activity starts {@link DetailActivity} with a "title" extra
 *  - the fragment tap counter is saved in instance state
 *  - lifecycle is logged under tag {@code AcosFragments}
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosFragments";
    private static final String KEY_FRAGMENT = "fragment";

    private TextView state;
    private String current = "Alpha";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        state = (TextView) findViewById(R.id.state);
        if (savedInstanceState != null) {
            current = savedInstanceState.getString(KEY_FRAGMENT, current);
            Log.i(TAG, "restored fragment=" + current);
        }

        ((Button) findViewById(R.id.show_a)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showFragment("Alpha", true);
            }
        });
        ((Button) findViewById(R.id.show_b)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showFragment("Beta", true);
            }
        });
        ((Button) findViewById(R.id.back)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                popBackStack();
            }
        });
        ((Button) findViewById(R.id.detail)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Intent intent = new Intent(MainActivity.this, DetailActivity.class);
                intent.putExtra("title", current);
                startActivity(intent);
                Log.i(TAG, "start_detail title=" + current);
            }
        });

        // Initial content: replaced without a back-stack entry.
        showFragment(current, false);
        state.setText("current=" + current);
    }

    private void showFragment(String title, boolean addToBackStack) {
        current = title;
        FragmentTransaction tx = getFragmentManager().beginTransaction();
        tx.replace(R.id.container, SectionFragment.newInstance(title));
        if (addToBackStack) {
            tx.addToBackStack(title);
        }
        tx.commit();
        state.setText("current=" + title);
        Log.i(TAG, "show_fragment title=" + title + " back_stack=" + addToBackStack);
    }

    private void popBackStack() {
        boolean popped = getFragmentManager().popBackStackImmediate();
        state.setText("popped=" + popped);
        Log.i(TAG, "pop_back_stack popped=" + popped);
    }

    @Override
    public void onBackPressed() {
        if (getFragmentManager().getBackStackEntryCount() > 0) {
            popBackStack();
            return;
        }
        super.onBackPressed();
    }

    @Override
    protected void onSaveInstanceState(Bundle outState) {
        super.onSaveInstanceState(outState);
        outState.putString(KEY_FRAGMENT, current);
        Log.i(TAG, "onSaveInstanceState fragment=" + current);
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "onResume");
    }
}
