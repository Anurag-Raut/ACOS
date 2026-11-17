package dev.acos.fixtures.multipkg;

import android.app.Activity;
import android.content.Context;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.content.res.Resources;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

/**
 * MPROC1 step 1: an app that asks its {@link PackageManager} about a **second
 * installed package** and builds that package's {@link Context}.
 *
 * <p>Run with `acos-art launch --apk multi-package-probe.apk --extra-apk
 * counter.apk`. The session installs both packages (each at its own uid), so
 * the probe must see the real second package — its `PackageInfo`, its
 * `ApplicationInfo` (sourceDir/uid/dataDir/nativeLibraryDir) and a working
 * `createPackageContext` whose resources resolve the other APK's strings.
 *
 * <p>Every fact is printed as one `ACOSPROBE` line (the launch reply's `stdout`
 * carries them verbatim) and shown on screen, so the same result is readable
 * from pixels and from the tree:
 *
 * <pre>
 * ACOSPROBE ok pkg=dev.acos.fixtures.counter version=1.0 src=/data/app/counter.apk uid=10193
 * ACOSPROBE ctx pkg=dev.acos.fixtures.counter label=Counter app_name=Counter
 * ACOSPROBE uid 10193 -> [dev.acos.fixtures.counter]
 * ACOSPROBE done
 * </pre>
 *
 * <p>Nothing here is faked: a missing package prints `ACOSPROBE missing …`,
 * shows the same text on screen and does not claim success. `ACOSPROBE_MISSING`
 * is the honest shape this fixture is checked against when the second APK is
 * *not* installed.
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosMultiPkg";

    /** The package every run of this fixture asks about. */
    public static final String EXTRA_PACKAGE = "dev.acos.fixtures.counter";

    private TextView probeView;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        probeView = (TextView) findViewById(R.id.probe);
        ((Button) findViewById(R.id.reprobe)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Log.i(TAG, "reprobe");
                probeView.setText(probe());
            }
        });
        probeView.setText(probe());
    }

    /**
     * Probe the second package and return the on-screen summary. Each step logs
     * one `ACOSPROBE` line; the returned text is a compact form of the same
     * facts.
     */
    private String probe() {
        PackageManager pm = getPackageManager();
        StringBuilder screen = new StringBuilder();
        try {
            PackageInfo info = pm.getPackageInfo(EXTRA_PACKAGE, 0);
            ApplicationInfo app = info.applicationInfo;
            String line = "ACOSPROBE ok pkg=" + info.packageName
                    + " version=" + info.versionName
                    + " src=" + app.sourceDir
                    + " uid=" + app.uid;
            Log.i(TAG, line);
            System.out.println(line);
            screen.append(EXTRA_PACKAGE).append('\n')
                    .append("version ").append(info.versionName).append('\n')
                    .append("src ").append(app.sourceDir).append('\n')
                    .append("uid ").append(app.uid);

            // A second package's Context: the framework builds Resources from
            // the other APK's `sourceDir`, so this proves the staged APK is
            // readable in the guest and the per-package ApplicationInfo is
            // right. The label and the `app_name` string come from the *other*
            // APK's resources, never from this one.
            // Diagnostic evidence for the framework path: the ApplicationInfo
            // ActivityThread.getPackageInfo() itself asks for (its own flag set
            // is GET_SHARED_LIBRARY_FILES|MATCH_DEBUG_TRIAGED_MISSING), and the
            // ContextImpl's own view of what it built.
            ApplicationInfo direct = pm.getApplicationInfo(EXTRA_PACKAGE, 0);
            String aiLine = "ACOSPROBE ai pkg=" + direct.packageName
                    + " flags=0x" + Integer.toHexString(direct.flags)
                    + " src=" + direct.sourceDir;
            Log.i(TAG, aiLine);
            System.out.println(aiLine);
            int atFlags = PackageManager.GET_SHARED_LIBRARY_FILES | 0x00020000;
            ApplicationInfo atInfo = pm.getApplicationInfo(EXTRA_PACKAGE, atFlags);
            String atLine = "ACOSPROBE at-flags pkg=" + atInfo.packageName
                    + " src=" + atInfo.sourceDir;
            Log.i(TAG, atLine);
            System.out.println(atLine);
            Context other = createPackageContext(EXTRA_PACKAGE,
                    Context.CONTEXT_INCLUDE_CODE | Context.CONTEXT_IGNORE_SECURITY);
            Resources otherRes = other.getResources();
            int labelId = otherRes.getIdentifier("app_name", "string", EXTRA_PACKAGE);
            String otherAppName = labelId == 0 ? "?" : otherRes.getString(labelId);
            String label = other.getApplicationInfo().loadLabel(pm).toString();
            String ctxLine = "ACOSPROBE ctx appInfo=" + other.getApplicationInfo().packageName
                    + " pkg=" + other.getPackageName()
                    + " label=" + label
                    + " app_name=" + otherAppName;
            Log.i(TAG, ctxLine);
            System.out.println(ctxLine);
            screen.append('\n').append("context ").append(other.getPackageName())
                    .append('\n').append("resources ").append(otherAppName);

            String[] forUid = pm.getPackagesForUid(app.uid);
            StringBuilder names = new StringBuilder();
            if (forUid != null) {
                for (String name : forUid) {
                    if (names.length() > 0) {
                        names.append(',');
                    }
                    names.append(name);
                }
            }
            String uidLine = "ACOSPROBE uid " + app.uid + " -> [" + names + "]";
            Log.i(TAG, uidLine);
            System.out.println(uidLine);
            screen.append('\n').append("uid ").append(app.uid).append(" -> [").append(names)
                    .append(']');
        } catch (PackageManager.NameNotFoundException e) {
            String line = "ACOSPROBE missing " + EXTRA_PACKAGE;
            Log.i(TAG, line);
            System.out.println(line);
            screen.setLength(0);
            screen.append(line);
        } catch (Throwable t) {
            String line = "ACOSPROBE error " + t.getClass().getName()
                    + (t.getMessage() == null ? "" : ": " + t.getMessage());
            Log.i(TAG, line);
            System.out.println(line);
            screen.setLength(0);
            screen.append(line);
        }
        System.out.println("ACOSPROBE done");
        Log.i(TAG, "ACOSPROBE done");
        return screen.toString();
    }
}
