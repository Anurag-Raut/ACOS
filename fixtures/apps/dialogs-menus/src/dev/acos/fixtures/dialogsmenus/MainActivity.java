package dev.acos.fixtures.dialogsmenus;

import android.app.Activity;
import android.app.AlertDialog;
import android.app.DatePickerDialog;
import android.content.DialogInterface;
import android.os.Bundle;
import android.util.Log;
import android.view.Menu;
import android.view.MenuItem;
import android.view.View;
import android.widget.AdapterView;
import android.widget.ArrayAdapter;
import android.widget.Button;
import android.widget.DatePicker;
import android.widget.PopupMenu;
import android.widget.Spinner;
import android.widget.TextView;
import android.widget.Toast;

/**
 * Dialogs-and-menus fixture: {@link AlertDialog}, {@link PopupMenu},
 * {@link Toast}, {@link DatePickerDialog} and a {@link Spinner}. Plain
 * framework APIs only (no AndroidX).
 *
 * Observable behaviour:
 *  - the AlertDialog button shows a two-button dialog; OK sets status to
 *    "Dialog confirmed" and logs {@code dialog_ok}
 *  - the PopupMenu button opens a menu anchored on the button; Rename/Delete
 *    set the status and log {@code popup_*}
 *  - the Toast button shows a toast and logs {@code toast_shown}
 *  - the Pick date button opens a DatePickerDialog; Set logs {@code date_set}
 *  - selecting a Spinner row logs {@code spinner_pos=N}
 *  - lifecycle is logged under tag {@code AcosDialogs}
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosDialogs";
    private static final int POPUP_RENAME = 1;
    private static final int POPUP_DELETE = 2;

    private TextView status;
    private Spinner spinner;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        status = (TextView) findViewById(R.id.status);

        ((Button) findViewById(R.id.dialog)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showAlert();
            }
        });
        ((Button) findViewById(R.id.popup)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showPopup(v);
            }
        });
        ((Button) findViewById(R.id.toast)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Toast.makeText(MainActivity.this, "Hello from ACOS", Toast.LENGTH_SHORT).show();
                Log.i(TAG, "toast_shown");
            }
        });
        ((Button) findViewById(R.id.date)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showDatePicker();
            }
        });

        spinner = (Spinner) findViewById(R.id.spinner);
        ArrayAdapter<String> adapter = new ArrayAdapter<String>(this,
                android.R.layout.simple_spinner_item,
                new String[] {"One", "Two", "Three"});
        spinner.setAdapter(adapter);
        spinner.setOnItemSelectedListener(new AdapterView.OnItemSelectedListener() {
            @Override
            public void onItemSelected(AdapterView<?> parent, View view, int position, long id) {
                Log.i(TAG, "spinner_pos=" + position);
            }

            @Override
            public void onNothingSelected(AdapterView<?> parent) {
            }
        });
    }

    private void showAlert() {
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

    private void showPopup(View anchor) {
        PopupMenu popup = new PopupMenu(this, anchor);
        Menu menu = popup.getMenu();
        menu.add(Menu.NONE, POPUP_RENAME, Menu.NONE, "Rename");
        menu.add(Menu.NONE, POPUP_DELETE, Menu.NONE, "Delete");
        popup.setOnMenuItemClickListener(new PopupMenu.OnMenuItemClickListener() {
            @Override
            public boolean onMenuItemClick(MenuItem item) {
                switch (item.getItemId()) {
                    case POPUP_RENAME:
                        status.setText("Renamed");
                        Log.i(TAG, "popup_rename");
                        return true;
                    case POPUP_DELETE:
                        status.setText("Deleted");
                        Log.i(TAG, "popup_delete");
                        return true;
                    default:
                        return false;
                }
            }
        });
        popup.show();
        Log.i(TAG, "popup_shown");
    }

    private void showDatePicker() {
        new DatePickerDialog(this, new DatePickerDialog.OnDateSetListener() {
            @Override
            public void onDateSet(DatePicker view, int year, int month, int day) {
                String text = "date " + year + "-" + (month + 1) + "-" + day;
                status.setText(text);
                Log.i(TAG, "date_set " + text);
            }
        }, 2024, 0, 15).show();
        Log.i(TAG, "date_dialog_shown");
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "onResume");
    }
}
