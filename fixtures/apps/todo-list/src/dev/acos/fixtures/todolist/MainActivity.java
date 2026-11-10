package dev.acos.fixtures.todolist;

import android.app.Activity;
import android.content.SharedPreferences;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.AdapterView;
import android.widget.ArrayAdapter;
import android.widget.Button;
import android.widget.EditText;
import android.widget.ListView;

import java.util.ArrayList;
import java.util.List;

/**
 * Todo-list fixture: an EditText + ListView (framework ListView, no
 * RecyclerView/AndroidX) with add/remove and SharedPreferences persistence.
 *
 * Observable behaviour:
 *  - Button id=add appends the EditText id=input text to ListView id=list
 *  - tapping a list row removes that item
 *  - Button id=clear empties the list
 *  - the list is saved to SharedPreferences in onPause and reloaded in onCreate,
 *    so it survives rotation and process restart
 *  - lifecycle/actions are logged with tag "AcosTodo"
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosTodo";
    private static final String PREFS = "todos";
    private static final String KEY_ITEMS = "items";

    private final List<String> items = new ArrayList<String>();
    private ArrayAdapter<String> adapter;
    private EditText input;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        input = (EditText) findViewById(R.id.input);
        ListView list = (ListView) findViewById(R.id.list);
        adapter = new ArrayAdapter<String>(this, android.R.layout.simple_list_item_1, items);
        list.setAdapter(adapter);
        list.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                String removed = items.remove(position);
                adapter.notifyDataSetChanged();
                Log.i(TAG, "remove item=" + removed + " size=" + items.size());
            }
        });
        ((Button) findViewById(R.id.add)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                String text = input.getText().toString().trim();
                if (text.length() == 0) {
                    Log.i(TAG, "add ignored empty");
                    return;
                }
                items.add(text);
                adapter.notifyDataSetChanged();
                input.setText("");
                Log.i(TAG, "add item=" + text + " size=" + items.size());
            }
        });
        ((Button) findViewById(R.id.clear)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                items.clear();
                adapter.notifyDataSetChanged();
                Log.i(TAG, "clear size=0");
            }
        });
        load();
    }

    private void load() {
        SharedPreferences prefs = getSharedPreferences(PREFS, MODE_PRIVATE);
        String saved = prefs.getString(KEY_ITEMS, "");
        if (saved.length() > 0) {
            String[] parts = saved.split("\n");
            for (String part : parts) {
                if (part.length() > 0) {
                    items.add(part);
                }
            }
            adapter.notifyDataSetChanged();
            Log.i(TAG, "loaded count=" + items.size());
        }
    }

    private void save() {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < items.size(); i++) {
            if (i > 0) {
                sb.append('\n');
            }
            sb.append(items.get(i));
        }
        getSharedPreferences(PREFS, MODE_PRIVATE).edit()
                .putString(KEY_ITEMS, sb.toString()).commit();
        Log.i(TAG, "saved count=" + items.size());
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
