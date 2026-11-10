package dev.acos.fixtures.listscroll;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.AdapterView;
import android.widget.ArrayAdapter;
import android.widget.ListView;
import android.widget.TextView;

/**
 * List fixture: a {@link ListView} backed by an {@link ArrayAdapter}, with
 * selection and programmatic scrolling. Plain framework APIs only.
 *
 * Observable behaviour:
 *  - the list is populated with 50 rows ("Row 0".."Row 49")
 *  - tapping a row sets the header TextView to "Selected: Row N" (tag AcosList)
 *  - {@link #scrollToLast()} scrolls to the final row
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosList";
    private static final int ROWS = 50;

    private ListView list;
    private TextView selected;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        selected = (TextView) findViewById(R.id.selected);
        list = (ListView) findViewById(R.id.list);

        String[] items = new String[ROWS];
        for (int i = 0; i < ROWS; i++) {
            items[i] = "Row " + i;
        }
        ArrayAdapter<String> adapter =
                new ArrayAdapter<String>(this, android.R.layout.simple_list_item_1, items);
        list.setAdapter(adapter);
        list.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                String text = "Selected: Row " + position;
                selected.setText(text);
                Log.i(TAG, "item_click position=" + position);
            }
        });
        Log.i(TAG, "adapter_count=" + adapter.getCount());
    }

    /** Scroll to the last row (exercises {@code smoothScrollToPosition}). */
    public void scrollToLast() {
        if (list != null) {
            list.smoothScrollToPosition(ROWS - 1);
            Log.i(TAG, "scroll_to=" + (ROWS - 1));
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "onResume");
    }
}
