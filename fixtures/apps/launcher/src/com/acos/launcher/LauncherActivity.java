package com.acos.launcher;

import android.app.Activity;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Comparator;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Set;

/**
 * LAUNCHER2: minimal guest Launcher system app (`MAIN`/`HOME`).
 *
 * Queries {@link PackageManager#queryIntentActivities} for {@code ACTION_MAIN}
 * + {@code CATEGORY_LAUNCHER}, renders each launchable app with its resolved
 * {@link ResolveInfo#loadLabel(PackageManager)} label as a clickable row, and
 * launches the tapped package via {@link PackageManager#getLaunchIntentForPackage}.
 */
public class LauncherActivity extends Activity {
    private static final String TAG = "AcosLauncher";

    private static final int[] ROW_IDS = {
        R.id.app_row_0, R.id.app_row_1, R.id.app_row_2, R.id.app_row_3,
        R.id.app_row_4, R.id.app_row_5, R.id.app_row_6, R.id.app_row_7,
        R.id.app_row_8, R.id.app_row_9, R.id.app_row_10, R.id.app_row_11,
        R.id.app_row_12, R.id.app_row_13, R.id.app_row_14, R.id.app_row_15,
    };

    private static final class AppEntry {
        final String packageName;
        final String activityName;
        final String label;

        AppEntry(String packageName, String activityName, String label) {
            this.packageName = packageName;
            this.activityName = activityName;
            this.label = label;
        }
    }

    private LinearLayout appList;
    private TextView emptyView;
    private String lastSignature = "";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.launcher);
        appList = (LinearLayout) findViewById(R.id.app_list);
        emptyView = (TextView) findViewById(R.id.launcher_empty);
        refreshApps();
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        Log.i(TAG, "ACOSPROBE launcher home");
        refreshApps();
    }

    @Override
    protected void onResume() {
        super.onResume();
        refreshApps();
    }

    private void refreshApps() {
        PackageManager pm = getPackageManager();
        Intent query = new Intent(Intent.ACTION_MAIN);
        query.addCategory(Intent.CATEGORY_LAUNCHER);
        List<ResolveInfo> resolved = pm.queryIntentActivities(query, 0);

        List<AppEntry> entries = new ArrayList<>();
        Set<String> seen = new HashSet<>();
        String selfPkg = getPackageName();
        if (resolved != null) {
            for (ResolveInfo ri : resolved) {
                if (ri == null || ri.activityInfo == null) continue;
                ActivityInfo ai = ri.activityInfo;
                if (ai.packageName == null || ai.name == null) continue;
                if (selfPkg.equals(ai.packageName)) continue;
                String key = ai.packageName + "/" + ai.name;
                if (!seen.add(key)) continue;
                CharSequence rawLabel = ri.loadLabel(pm);
                String label = rawLabel == null ? "" : rawLabel.toString().trim();
                if (label.isEmpty()) {
                    label = ai.packageName;
                }
                entries.add(new AppEntry(ai.packageName, ai.name, label));
            }
        }

        Collections.sort(entries, new Comparator<AppEntry>() {
            @Override
            public int compare(AppEntry a, AppEntry b) {
                int cmp = a.label.toLowerCase(Locale.ROOT)
                        .compareTo(b.label.toLowerCase(Locale.ROOT));
                if (cmp != 0) return cmp;
                cmp = a.packageName.compareTo(b.packageName);
                if (cmp != 0) return cmp;
                return a.activityName.compareTo(b.activityName);
            }
        });

        StringBuilder sig = new StringBuilder();
        for (int i = 0; i < entries.size(); i++) {
            if (i > 0) sig.append("|");
            AppEntry e = entries.get(i);
            sig.append(e.packageName).append("/").append(e.activityName).append("=").append(e.label);
        }
        String signature = sig.toString();
        if (signature.equals(lastSignature)) {
            return;
        }
        lastSignature = signature;

        appList.removeAllViews();
        if (entries.isEmpty()) {
            emptyView.setVisibility(View.VISIBLE);
        } else {
            emptyView.setVisibility(View.GONE);
            for (int i = 0; i < entries.size(); i++) {
                final AppEntry entry = entries.get(i);
                Button row = new Button(this);
                if (i < ROW_IDS.length) {
                    row.setId(ROW_IDS[i]);
                } else {
                    row.setId(View.generateViewId());
                }
                row.setText(entry.label);
                row.setContentDescription(entry.label + " (" + entry.packageName + ")");
                LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(
                        LinearLayout.LayoutParams.MATCH_PARENT,
                        LinearLayout.LayoutParams.WRAP_CONTENT);
                lp.topMargin = 12;
                row.setLayoutParams(lp);
                row.setOnClickListener(new View.OnClickListener() {
                    @Override
                    public void onClick(View v) {
                        launchApp(entry);
                    }
                });
                appList.addView(row);
            }
        }
        Log.i(TAG, "ACOSPROBE launcher listed=" + entries.size() + " apps=" + signature);
    }

    private void launchApp(AppEntry entry) {
        PackageManager pm = getPackageManager();
        Intent launchIntent = pm.getLaunchIntentForPackage(entry.packageName);
        if (launchIntent == null) {
            launchIntent = new Intent(Intent.ACTION_MAIN);
            launchIntent.addCategory(Intent.CATEGORY_LAUNCHER);
            launchIntent.setClassName(entry.packageName, entry.activityName);
            launchIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        } else {
            launchIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        }
        Log.i(TAG, "ACOSPROBE launcher launch pkg=" + entry.packageName
                + " activity=" + entry.activityName + " label=" + entry.label);
        startActivity(launchIntent);
    }
}
