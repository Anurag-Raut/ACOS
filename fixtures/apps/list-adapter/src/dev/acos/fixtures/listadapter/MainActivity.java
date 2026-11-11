package dev.acos.fixtures.listadapter;

import android.app.Activity;
import android.content.Context;
import android.os.Bundle;
import android.util.Log;
import android.view.LayoutInflater;
import android.view.View;
import android.view.ViewGroup;
import android.widget.AdapterView;
import android.widget.BaseAdapter;
import android.widget.Button;
import android.widget.ListView;
import android.widget.TextView;

/**
 * Recycler-like list fixture: a {@link ListView} backed by a hand-written
 * {@link BaseAdapter} (not {@code ArrayAdapter}), a custom two-line row layout,
 * item-click handling and programmatic scrolling. Plain framework APIs only.
 *
 * Observable behaviour:
 *  - the list has {@value #ROWS} rows, each "Item N" / "subtitle N"
 *  - tapping row N sets the header to "Selected: Item N" and logs
 *    {@code item_click position=N}
 *  - the scroll button calls {@code smoothScrollToPosition(ROWS-1)} and logs
 *    {@code scroll_to=<ROWS-1>}
 *  - lifecycle is logged under tag {@code AcosListAdapter}
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosListAdapter";
    private static final int ROWS = 60;

    private ListView list;
    private TextView selected;
    private ItemAdapter adapter;

    static final class Item {
        final String title;
        final String sub;

        Item(String title, String sub) {
            this.title = title;
            this.sub = sub;
        }
    }

    /** Hand-written adapter: two-line rows inflated from res/layout/row.xml. */
    static final class ItemAdapter extends BaseAdapter {
        private final Context context;
        private final Item[] items;

        ItemAdapter(Context context, Item[] items) {
            this.context = context;
            this.items = items;
        }

        @Override
        public int getCount() {
            return items.length;
        }

        @Override
        public Object getItem(int position) {
            return items[position];
        }

        @Override
        public long getItemId(int position) {
            return position;
        }

        @Override
        public View getView(int position, View convertView, ViewGroup parent) {
            View view = convertView;
            if (view == null) {
                view = LayoutInflater.from(context).inflate(R.layout.row, parent, false);
            }
            Item item = items[position];
            ((TextView) view.findViewById(R.id.row_title)).setText(item.title);
            ((TextView) view.findViewById(R.id.row_sub)).setText(item.sub);
            return view;
        }
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        selected = (TextView) findViewById(R.id.selected);
        list = (ListView) findViewById(R.id.list);

        Item[] items = new Item[ROWS];
        for (int i = 0; i < ROWS; i++) {
            items[i] = new Item("Item " + i, "subtitle " + i);
        }
        adapter = new ItemAdapter(this, items);
        list.setAdapter(adapter);
        list.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                selected.setText("Selected: Item " + position);
                Log.i(TAG, "item_click position=" + position);
            }
        });
        ((Button) findViewById(R.id.scroll)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                list.smoothScrollToPosition(ROWS - 1);
                Log.i(TAG, "scroll_to=" + (ROWS - 1));
            }
        });
        Log.i(TAG, "adapter_count=" + adapter.getCount());
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "onResume");
    }
}
