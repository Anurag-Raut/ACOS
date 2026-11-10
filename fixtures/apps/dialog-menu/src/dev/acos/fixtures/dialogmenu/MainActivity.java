package dev.acos.fixtures.dialogmenu;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.os.Bundle;
import android.util.Log;
import android.view.Menu;
import android.view.MenuItem;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;
import android.widget.Toast;

/**
 * Dialog/menu fixture: {@link AlertDialog}, {@link Toast} and an options menu.
 * Plain framework APIs only.
 *
 * Observable behaviour:
 *  - the dialog button shows an AlertDialog; OK sets the status TextView to
 *    "Dialog confirmed" and logs {@code dialog_ok}
 *  - the toast button shows a Toast and logs {@code toast_shown}
 *  - the options menu has Reset and About items (tag AcosDialog)
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosDialog";
    private static final int MENU_RESET = 1;
    private static final int MENU_ABOUT = 2;

    private TextView status;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        status = (TextView) findViewById(R.id.status);

        ((Button) findViewById(R.id.dialog)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showDialog();
            }
        });
        ((Button) findViewById(R.id.toast)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Toast.makeText(MainActivity.this, "Hello from ACOS", Toast.LENGTH_SHORT).show();
                Log.i(TAG, "toast_shown");
            }
        });
    }

    private void showDialog() {
        new AlertDialog.Builder(this)
                .setTitle("Confirm")
                .setMessage("Are you sure?")
                .setPositiveButton("OK", new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        status.setText("Dialog confirmed");
                        Log.i(TAG, "dialog_ok which=" + which);
                    }
                })
                .setNegativeButton("Cancel", null)
                .show();
        Log.i(TAG, "dialog_shown");
    }

    @Override
    public boolean onCreateOptionsMenu(Menu menu) {
        menu.add(Menu.NONE, MENU_RESET, Menu.NONE, R.string.menu_reset);
        menu.add(Menu.NONE, MENU_ABOUT, Menu.NONE, R.string.menu_about);
        Log.i(TAG, "options_menu_created size=" + menu.size());
        return true;
    }

    @Override
    public boolean onOptionsItemSelected(MenuItem item) {
        switch (item.getItemId()) {
            case MENU_RESET:
                status.setText(R.string.idle);
                Log.i(TAG, "menu_reset");
                return true;
            case MENU_ABOUT:
                status.setText("About");
                Log.i(TAG, "menu_about");
                return true;
            default:
                return super.onOptionsItemSelected(item);
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "onResume");
    }
}
