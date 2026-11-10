package dev.acos.fixtures.formvalidation;

import android.app.Activity;
import android.app.AlertDialog;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;
import android.widget.Toast;

/**
 * Form-validation fixture: two EditTexts, inline error TextView, Toast on
 * failure and an AlertDialog on success. Plain framework APIs only.
 *
 * Observable behaviour:
 *  - Button id=submit with an empty name shows error "Name is required"
 *  - with a name but empty email shows "Email is required"
 *  - with a malformed email shows "Invalid email"
 *  - with a valid name + email shows an AlertDialog "Submitted: <name>"
 *  - lifecycle/validation is logged with tag "AcosForm"
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosForm";

    private EditText name;
    private EditText email;
    private TextView error;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        name = (EditText) findViewById(R.id.name);
        email = (EditText) findViewById(R.id.email);
        error = (TextView) findViewById(R.id.error);
        ((Button) findViewById(R.id.submit)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                submit();
            }
        });
    }

    private void submit() {
        String n = name.getText().toString().trim();
        String e = email.getText().toString().trim();
        if (n.length() == 0) {
            fail("Name is required", "name");
            return;
        }
        if (e.length() == 0) {
            fail("Email is required", "email");
            return;
        }
        if (!e.contains("@") || !e.contains(".")) {
            fail("Invalid email", "email_format");
            return;
        }
        error.setText("");
        Log.i(TAG, "validation_ok name=" + n + " email=" + e);
        Toast.makeText(this, "Submitted", Toast.LENGTH_SHORT).show();
        new AlertDialog.Builder(this)
                .setTitle("Success")
                .setMessage("Submitted: " + n)
                .setPositiveButton("OK", null)
                .show();
    }

    private void fail(String message, String reason) {
        error.setText(message);
        Log.i(TAG, "validation_failed reason=" + reason + " message=" + message);
        Toast.makeText(this, message, Toast.LENGTH_SHORT).show();
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
