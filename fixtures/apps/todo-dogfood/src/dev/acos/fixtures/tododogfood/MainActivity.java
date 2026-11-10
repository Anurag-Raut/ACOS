package dev.acos.fixtures.tododogfood;

import android.app.Activity;
import android.content.SharedPreferences;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.AdapterView;
import android.widget.ArrayAdapter;
import android.widget.Button;
import android.widget.ListView;
import android.widget.TextView;

import java.util.ArrayList;
import java.util.List;

/**
 * Counter-with-history dogfood fixture.
 *
 * A small app that an AI agent could plausibly write and then verify:
 *
 *  - TextView id=count        shows the current count ("Count: N")
 *  - Button id=inc / id=dec   increment / decrement, update the TextView
 *  - Button id=reset          set the count back to 0
 *  - Button id=snapshot       append "count=N" to the history ListView
 *  - ListView id=history_list tapping a row removes that snapshot
 *  - the count and history are saved to SharedPreferences on onPause and
 *    restored in onCreate, so they survive a process restart
 *
 * Every state change is logged with tag "AcosTodoDogfood" so an agent can
 * observe the run without a frame.
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosTodoDogfood";
    private static final String PREFS = "counter";
    private static final String KEY_COUNT = "count";
    private static final String KEY_HISTORY = "history";

    private final List<String> history = new ArrayList<String>();
    private int count;
    private TextView countView;
    private ArrayAdapter<String> adapter;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);

        countView = (TextView) findViewById(R.id.count);
        ListView list = (ListView) findViewById(R.id.history_list);
        adapter = new ArrayAdapter<String>(this, android.R.layout.simple_list_item_1, history);
        list.setAdapter(adapter);
        list.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                String removed = history.remove(position);
                adapter.notifyDataSetChanged();
                Log.i(TAG, "history remove=" + removed + " size=" + history.size());
            }
        });

        ((Button) findViewById(R.id.inc)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                count++;
                render();
                Log.i(TAG, "inc count=" + count);
            }
        });
        ((Button) findViewById(R.id.dec)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                count--;
                render();
                Log.i(TAG, "dec count=" + count);
            }
        });
        ((Button) findViewById(R.id.reset)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                count = 0;
                render();
                Log.i(TAG, "reset count=0");
            }
        });
        ((Button) findViewById(R.id.snapshot)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                history.add("count=" + count);
                adapter.notifyDataSetChanged();
                Log.i(TAG, "snapshot count=" + count + " size=" + history.size());
            }
        });

        load();
        render();
    }

    private void render() {
        countView.setText(getString(R.string.count_fmt, count));
    }

    private void load() {
        SharedPreferences prefs = getSharedPreferences(PREFS, MODE_PRIVATE);
        count = prefs.getInt(KEY_COUNT, 0);
        String saved = prefs.getString(KEY_HISTORY, "");
        if (saved.length() > 0) {
            for (String part : saved.split("\n")) {
                if (part.length() > 0) {
                    history.add(part);
                }
            }
            adapter.notifyDataSetChanged();
        }
        Log.i(TAG, "loaded count=" + count + " history=" + history.size());
    }

    private void save() {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < history.size(); i++) {
            if (i > 0) {
                sb.append('\n');
            }
            sb.append(history.get(i));
        }
        getSharedPreferences(PREFS, MODE_PRIVATE).edit()
                .putInt(KEY_COUNT, count)
                .putString(KEY_HISTORY, sb.toString())
                .commit();
        Log.i(TAG, "saved count=" + count + " history=" + history.size());
    }

    @Override
    protected void onStart() {
        super.onStart();
        Log.i(TAG, "onStart");
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "onResume");
    }

    @Override
    protected void onPause() {
        super.onPause();
        Log.i(TAG, "onPause");
        save();
    }

    @Override
    protected void onStop() {
        super.onStop();
        Log.i(TAG, "onStop");
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        Log.i(TAG, "onDestroy");
    }
}
