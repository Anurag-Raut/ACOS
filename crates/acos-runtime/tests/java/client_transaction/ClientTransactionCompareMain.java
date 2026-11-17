import android.app.ActivityOptions;
import android.app.ResultInfo;
import android.app.servertransaction.ActivityResultItem;
import android.app.servertransaction.ClientTransaction;
import android.app.servertransaction.DestroyActivityItem;
import android.app.servertransaction.LaunchActivityItem;
import android.app.servertransaction.NewIntentItem;
import android.app.servertransaction.PauseActivityItem;
import android.app.servertransaction.ResumeActivityItem;
import android.app.servertransaction.StopActivityItem;
import android.content.ComponentName;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.res.Configuration;
import android.graphics.Rect;
import android.os.Binder;
import android.os.Bundle;
import android.os.LocaleList;
import android.os.Parcel;
import android.os.Parcelable;

import com.android.internal.content.ReferrerIntent;

import java.lang.reflect.Field;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

/**
 * MPROC6 real-Java differential for the host-built launch transactions.
 *
 * <p>Runs on the real Android 14 framework through the ACOS VM (the same harness
 * as {@code ParcelableCompareMain}). For every {@code ClientTransaction} item the
 * host encodes, this builds the <em>same</em> logical value out of the real
 * framework classes, calls the real {@code writeToParcel} into an ACOS-native
 * {@code Parcel} and prints the marshalled bytes:
 *
 * <pre>write.&lt;name&gt;=&lt;hex&gt;</pre>
 *
 * <p>Given {@code <name>=<hex>} arguments it unmarshalls the Rust bytes, calls the
 * real {@code CREATOR.createFromParcel}, checks the reconstructed fields (the
 * intent extras, the pending result, the lifecycle flags) and re-emits the
 * object, printing:
 *
 * <pre>read.&lt;name&gt;=ok</pre>
 * <pre>roundtrip.&lt;name&gt;=&lt;hex&gt;</pre>
 *
 * <p>The Rust test compares both directions byte-for-byte
 * ({@code tests/client_transaction_java.rs}).
 */
public final class ClientTransactionCompareMain {

    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    static Field field(Class<?> c, String name) throws Exception {
        for (Class<?> k = c; k != null; k = k.getSuperclass()) {
            for (Field f : k.getDeclaredFields()) {
                if (f.getName().equals(name)) {
                    f.setAccessible(true);
                    return f;
                }
            }
        }
        throw new NoSuchFieldException(c.getName() + "." + name);
    }

    static Object get(Object o, String name) throws Exception {
        return field(o.getClass(), name).get(o);
    }

    static void set(Object o, String name, Object v) throws Exception {
        field(o.getClass(), name).set(o, v);
    }

    interface Writer {
        void write(Parcel p) throws Exception;
    }

    static void emit(String name, Writer w) throws Exception {
        Parcel p = Parcel.obtain();
        try {
            w.write(p);
            System.out.println("write." + name + "=" + hex(p.marshall()));
        } finally {
            p.recycle();
        }
    }

    static String hex(byte[] bytes) {
        StringBuilder sb = new StringBuilder(bytes.length * 2);
        for (byte b : bytes) {
            sb.append(String.format("%02x", b & 0xff));
        }
        return sb.toString();
    }

    static byte[] unhex(String hex) {
        check(hex.length() % 2 == 0, "hex length");
        byte[] out = new byte[hex.length() / 2];
        for (int i = 0; i < out.length; i++) {
            out[i] = (byte) Integer.parseInt(hex.substring(i * 2, i * 2 + 2), 16);
        }
        return out;
    }

    // -----------------------------------------------------------------------
    // the logical values the host encodes
    // -----------------------------------------------------------------------

    static final String PKG = "dev.acos.fixtures.multiactivity";
    static final String CLS = "dev.acos.fixtures.multiactivity.SecondActivity";

    /**
     * The intent the ACOS host records for a launcher start with extras: an
     * explicit component, a MAIN action and a string, an int, a boolean and a
     * parcelable (a {@code Rect}) extra.
     */
    static Intent intent() {
        Intent intent = new Intent();
        intent.setAction("android.intent.action.MAIN");
        intent.setClassName(PKG, CLS);
        intent.setFlags(0x10000000);
        intent.addCategory("android.intent.category.LAUNCHER");
        intent.putExtra("greeting", "hello");
        intent.putExtra("count", 7);
        intent.putExtra("flag", true);
        intent.putExtra("bounds", new Rect(1, 2, 3, 4));
        return intent;
    }

    /**
     * LAUNCHEXTRAS: the session-start Intent {@code AcosServices::client_transaction::launch_intent}
     * encodes for a scenario that declares an explicit action, a data URI and
     * one extra of every supported Java type.
     *
     * <p>The data string goes through the framework's own `Uri.parse`, so the
     * bytes carry whatever this image's Uri implementation writes (a
     * `StringUri`: type tag 1 + the string) — the host encoder must match it,
     * not a remembered source shape.
     */
    static Intent launchIntentExtras() {
        Intent intent = new Intent();
        intent.setAction("dev.acos.fixtures.launchextras.ACTION");
        intent.setData(android.net.Uri.parse("acos://launch/extras?source=scenario"));
        intent.setClassName("dev.acos.fixtures.launchextras",
                "dev.acos.fixtures.launchextras.MainActivity");
        intent.setFlags(0x10000000);
        intent.putExtra("probe_string", "hello");
        intent.putExtra("probe_int", 42);
        intent.putExtra("probe_long", 5000000000L);
        intent.putExtra("probe_boolean", true);
        intent.putExtra("probe_float", 1.5f);
        intent.putExtra("probe_double", 0.25d);
        intent.putExtra("probe_string_array", new String[]{"alpha", "beta"});
        return intent;
    }

    static void checkLaunchIntentExtras(Intent intent) {
        check(intent.getAction().equals("dev.acos.fixtures.launchextras.ACTION"), "action");
        check(intent.getDataString().equals("acos://launch/extras?source=scenario"), "data");
        check(intent.getComponent().getClassName().equals(
                "dev.acos.fixtures.launchextras.MainActivity"), "component");
        check(intent.getFlags() == 0x10000000, "flags");
        check("hello".equals(intent.getStringExtra("probe_string")), "string extra");
        check(intent.getIntExtra("probe_int", -1) == 42, "int extra");
        check(intent.getLongExtra("probe_long", -1) == 5000000000L, "long extra");
        check(intent.getBooleanExtra("probe_boolean", false), "boolean extra");
        check(intent.getFloatExtra("probe_float", -1f) == 1.5f, "float extra");
        check(intent.getDoubleExtra("probe_double", -1d) == 0.25d, "double extra");
        String[] array = intent.getStringArrayExtra("probe_string_array");
        check(array != null && array.length == 2 && array[0].equals("alpha")
                && array[1].equals("beta"), "string array extra");
    }

    /** `bind_app::build_config`: locale en-US, 420 dpi, 411x842 dp, portrait. */
    static Configuration configuration() throws Exception {
        Configuration c = new Configuration();
        c.setLocales(LocaleList.forLanguageTags("en-US"));
        c.densityDpi = 420;
        c.screenWidthDp = 411;
        c.screenHeightDp = 842;
        c.smallestScreenWidthDp = 411;
        c.fontScale = 1.0f;
        c.orientation = Configuration.ORIENTATION_PORTRAIT;
        // Exactly the setters `bind_app::build_config` performs on a fresh
        // `Configuration`; every other field keeps the framework's own
        // constructor default (which is what `bindApplication` delivers today).
        return c;
    }

    static ActivityInfo activityInfo() {
        ActivityInfo info = new ActivityInfo();
        info.name = CLS;
        info.packageName = PKG;
        info.processName = PKG;
        info.applicationInfo = null;
        return info;
    }

    static ResultInfo resultInfo() {
        return new ResultInfo("dev.acos.fixtures.multiactivity.MainActivity", 5,
                android.app.Activity.RESULT_OK, intent());
    }

    /** The `ActivityInfo` the launch item carries (the Rust PMS codec's bytes
     * when the harness supplied them: the item's field order is what is under
     * test, and `ActivityInfo`/`ApplicationInfo` have their own differential). */
    static ActivityInfo launchInfo;

    static LaunchActivityItem launchItem() throws Exception {
        return launchItem(null);
    }

    /** The launch item; `pendingResults` carries the pending `ActivityResultItem`
     * result when non-null. The binder tokens are null in this differential:
     * a real binder's id is minted by the transport, not by the codec (the
     * non-null token path is covered by the host's own ATMS tests). */
    static LaunchActivityItem launchItem(List<ResultInfo> pendingResults) throws Exception {
        return LaunchActivityItem.obtain(intent(), 0, launchInfo, configuration(),
                configuration(), 0, PKG, null, 2, null, null, pendingResults, null,
                (ActivityOptions) null, false, null, null, null, null, false, null);
    }

    static ClientTransaction transaction() throws Exception {
        ClientTransaction tx = ClientTransaction.obtain(null, null);
        tx.addCallback(launchItem());
        tx.setLifecycleStateRequest(ResumeActivityItem.obtain(2, false, false));
        return tx;
    }

    /** A lifecycle-only transaction: what ATMS sends for pause/stop/resume. It
     * has no callbacks at all, so `writeToParcel` writes the presence `false`
     * and no list. */
    static ClientTransaction lifecycleTransaction() {
        ClientTransaction tx = ClientTransaction.obtain(null, null);
        tx.setLifecycleStateRequest(PauseActivityItem.obtain(false, true, 0, false, false));
        return tx;
    }

    static Parcelable activityResultItem() {
        List<ResultInfo> results = new ArrayList<>();
        results.add(resultInfo());
        return ActivityResultItem.obtain(results);
    }

    static Parcelable newIntentItem() {
        List<ReferrerIntent> intents = new ArrayList<>();
        intents.add(new ReferrerIntent(intent(), PKG));
        return NewIntentItem.obtain(intents, true);
    }

    static void checkIntent(Object item) throws Exception {
        Intent intent = (Intent) get(item, "mIntent");
        check(intent != null, "mIntent");
        check(intent.getAction().equals("android.intent.action.MAIN"), "action");
        check(intent.getComponent().equals(new ComponentName(PKG, CLS)), "component");
        check(intent.getFlags() == 0x10000000, "flags");
        check(intent.getCategories().contains("android.intent.category.LAUNCHER"), "categories");
        check("hello".equals(intent.getStringExtra("greeting")), "string extra");
        check(intent.getIntExtra("count", -1) == 7, "int extra");
        check(intent.getBooleanExtra("flag", false), "bool extra");
        Rect bounds = intent.getParcelableExtra("bounds");
        check(bounds != null && bounds.left == 1 && bounds.top == 2 && bounds.right == 3
                && bounds.bottom == 4, "parcelable extra: " + bounds);
    }

    public static void main(String[] args) throws Exception {
        // The launch item's `ActivityInfo` and the `ComposeState` values are the
        // only inputs: everything else is built independently on both sides.
        launchInfo = activityInfo();
        for (String arg : args) {
            int eq = arg.indexOf('=');
            if (eq < 0) {
                continue;
            }
            if (arg.substring(0, eq).equals("activity_info")) {
                byte[] bytes = unhex(arg.substring(eq + 1));
                Parcel p = Parcel.obtain();
                try {
                    p.unmarshall(bytes, 0, bytes.length);
                    p.setDataPosition(0);
                    launchInfo = ActivityInfo.CREATOR.createFromParcel(p);
                } finally {
                    p.recycle();
                }
            }
        }
        emit("intent", p -> intent().writeToParcel(p, 0));
        emit("launch_intent_extras", p -> launchIntentExtras().writeToParcel(p, 0));
        emit("configuration", p -> configuration().writeToParcel(p, 0));
        emit("activity_info", p -> launchInfo.writeToParcel(p, 0));
        emit("result_info", p -> resultInfo().writeToParcel(p, 0));
        emit("referrer_intent", p -> new ReferrerIntent(intent(), PKG).writeToParcel(p, 0));
        emit("launch_activity_item", p -> launchItem().writeToParcel(p, 0));
        emit("launch_activity_item_with_result", p -> {
            List<ResultInfo> pending = new ArrayList<>();
            pending.add(resultInfo());
            launchItem(pending).writeToParcel(p, 0);
        });
        emit("resume_activity_item",
                p -> ResumeActivityItem.obtain(2, false, false).writeToParcel(p, 0));
        emit("pause_activity_item",
                p -> PauseActivityItem.obtain(false, true, 0, false, false)
                        .writeToParcel(p, 0));
        emit("stop_activity_item", p -> StopActivityItem.obtain(0x400).writeToParcel(p, 0));
        emit("destroy_activity_item", p -> DestroyActivityItem.obtain(true, 0)
                .writeToParcel(p, 0));
        emit("activity_result_item", p -> activityResultItem().writeToParcel(p, 0));
        emit("new_intent_item", p -> newIntentItem().writeToParcel(p, 0));
        emit("client_transaction", p -> transaction().writeToParcel(p, 0));
        // MPROC7: a result delivery — `ActivityResultItem` callback +
        // `ResumeActivityItem` — the transaction ATMS sends to a caller whose
        // `startActivityForResult` returns.
        emit("client_transaction_result", p -> {
            ClientTransaction tx = ClientTransaction.obtain(null, null);
            tx.addCallback((android.app.servertransaction.ClientTransactionItem) activityResultItem());
            tx.setLifecycleStateRequest(ResumeActivityItem.obtain(2, false, false));
            tx.writeToParcel(p, 0);
        });
        emit("client_transaction_lifecycle_only",
                p -> lifecycleTransaction().writeToParcel(p, 0));

        for (String arg : args) {
            int eq = arg.indexOf('=');
            if (eq < 0) {
                continue;
            }
            String name = arg.substring(0, eq);
            byte[] bytes = unhex(arg.substring(eq + 1));
            Parcel p = Parcel.obtain();
            try {
                p.unmarshall(bytes, 0, bytes.length);
                p.setDataPosition(0);
                Parcelable got;
                switch (name) {
                    case "intent":
                        got = Intent.CREATOR.createFromParcel(p);
                        break;
                    case "launch_intent_extras": {
                        got = Intent.CREATOR.createFromParcel(p);
                        checkLaunchIntentExtras((Intent) got);
                        break;
                    }
                    case "configuration":
                        got = Configuration.CREATOR.createFromParcel(p);
                        break;
                    case "activity_info":
                        got = ActivityInfo.CREATOR.createFromParcel(p);
                        break;
                    case "result_info":
                        got = ResultInfo.CREATOR.createFromParcel(p);
                        break;
                    case "referrer_intent":
                        got = ReferrerIntent.CREATOR.createFromParcel(p);
                        break;
                    case "launch_activity_item":
                        got = LaunchActivityItem.CREATOR.createFromParcel(p);
                        checkIntent(got);
                        Object info = get(got, "mInfo");
                        check(((ActivityInfo) info).name.equals(CLS), "mInfo.name");
                        check(((ActivityInfo) info).packageName.equals(PKG), "mInfo.package");
                        Configuration cur = (Configuration) get(got, "mCurConfig");
                        check(cur.densityDpi == 420, "mCurConfig.densityDpi");
                        check(cur.orientation == Configuration.ORIENTATION_PORTRAIT,
                                "mCurConfig.orientation");
                        check(cur.getLocales().get(0).toLanguageTag().equals("en-US"),
                                "mCurConfig.locales");
                        Configuration override = (Configuration) get(got, "mOverrideConfig");
                        check(override.densityDpi == 420, "mOverrideConfig.densityDpi");
                        check(((Integer) get(got, "mProcState")) == 2, "mProcState");
                        check(((Boolean) get(got, "mIsForward")) == false, "mIsForward");
                        check(PKG.equals(get(got, "mReferrer")), "mReferrer");
                        check(get(got, "mTaskFragmentToken") == null, "mTaskFragmentToken");
                        break;
                    case "launch_activity_item_with_result": {
                        got = LaunchActivityItem.CREATOR.createFromParcel(p);
                        checkIntent(got);
                        List<?> pending = (List<?>) get(got, "mPendingResults");
                        check(pending.size() == 1, "one pending result");
                        ResultInfo result = (ResultInfo) pending.get(0);
                        check(get(result, "mRequestCode").equals(5), "pending requestCode");
                        Rect bounds = ((Intent) get(result, "mData")).getParcelableExtra("bounds");
                        check(bounds != null && bounds.bottom == 4,
                                "the pending result's intent extras survive");
                        break;
                    }
                    case "resume_activity_item":
                        got = ResumeActivityItem.CREATOR.createFromParcel(p);
                        check(((Integer) get(got, "mProcState")) == 2, "resume mProcState");
                        check(((Boolean) get(got, "mUpdateProcState")) == true,
                                "resume mUpdateProcState (obtain(IZZ) re-evaluates)");
                        break;
                    case "pause_activity_item":
                        got = PauseActivityItem.CREATOR.createFromParcel(p);
                        check(((Boolean) get(got, "mFinished")) == false, "pause mFinished");
                        check(((Boolean) get(got, "mUserLeaving")) == true, "pause mUserLeaving");
                        break;
                    case "stop_activity_item":
                        got = StopActivityItem.CREATOR.createFromParcel(p);
                        check(((Integer) get(got, "mConfigChanges")) == 0x400,
                                "stop mConfigChanges");
                        break;
                    case "destroy_activity_item":
                        got = DestroyActivityItem.CREATOR.createFromParcel(p);
                        check(((Boolean) get(got, "mFinished")) == true, "destroy mFinished");
                        break;
                    case "activity_result_item": {
                        got = ActivityResultItem.CREATOR.createFromParcel(p);
                        List<?> results = (List<?>) get(got, "mResultInfoList");
                        check(results.size() == 1, "one pending result");
                        ResultInfo result = (ResultInfo) results.get(0);
                        check(get(result, "mRequestCode").equals(5), "result requestCode");
                        Rect bounds = ((Intent) get(result, "mData")).getParcelableExtra("bounds");
                        check(bounds != null && bounds.bottom == 4,
                                "pending result data survives");
                        break;
                    }
                    case "new_intent_item": {
                        got = NewIntentItem.CREATOR.createFromParcel(p);
                        check(((Boolean) get(got, "mResume")) == true, "new intent mResume");
                        List<?> intents = (List<?>) get(got, "mIntents");
                        check(intents.size() == 1, "one new intent");
                        Intent first = (Intent) intents.get(0);
                        check("hello".equals(first.getStringExtra("greeting")),
                                "new intent extras survive");
                        check(PKG.equals(get(intents.get(0), "mReferrer")), "referrer");
                        break;
                    }
                    case "client_transaction": {
                        got = ClientTransaction.CREATOR.createFromParcel(p);
                        List<?> callbacks = (List<?>) get(got, "mActivityCallbacks");
                        check(callbacks.size() == 1, "one callback");
                        check(callbacks.get(0) instanceof LaunchActivityItem,
                                "callback is a LaunchActivityItem");
                        checkIntent(callbacks.get(0));
                        Object lifecycle = get(got, "mLifecycleStateRequest");
                        check(lifecycle instanceof ResumeActivityItem, "lifecycle request");
                        check(get(got, "mActivityCallbacks") != null,
                                "a transaction with a callback has a non-null callback list");
                        check(get(got, "mActivityToken") == null, "mActivityToken");
                        break;
                    }
                    case "client_transaction_result": {
                        got = ClientTransaction.CREATOR.createFromParcel(p);
                        List<?> callbacks = (List<?>) get(got, "mActivityCallbacks");
                        check(callbacks != null && callbacks.size() == 1, "one callback");
                        check(callbacks.get(0) instanceof ActivityResultItem,
                                "callback is an ActivityResultItem");
                        List<?> results = (List<?>) get(callbacks.get(0), "mResultInfoList");
                        check(results != null && results.size() == 1, "one pending result");
                        ResultInfo result = (ResultInfo) results.get(0);
                        check(get(result, "mRequestCode").equals(5), "result requestCode");
                        check(get(result, "mResultCode").equals(-1), "result resultCode");
                        Rect bounds = ((Intent) get(result, "mData")).getParcelableExtra("bounds");
                        check(bounds != null && bounds.bottom == 4,
                                "result data survives the client transaction");
                        Object lifecycle = get(got, "mLifecycleStateRequest");
                        check(lifecycle instanceof ResumeActivityItem, "lifecycle request");
                        break;
                    }
                    case "client_transaction_lifecycle_only": {
                        got = ClientTransaction.CREATOR.createFromParcel(p);
                        check(get(got, "mActivityCallbacks") == null,
                                "a lifecycle-only transaction has no callback list");
                        Object lifecycle = get(got, "mLifecycleStateRequest");
                        check(lifecycle instanceof PauseActivityItem, "lifecycle request");
                        check(((Boolean) get(lifecycle, "mFinished")) == false, "pause mFinished");
                        break;
                    }
                    default:
                        throw new IllegalArgumentException("unknown name " + name);
                }
                System.out.println("read." + name + "=ok");
                Parcel again = Parcel.obtain();
                try {
                    got.writeToParcel(again, 0);
                    System.out.println("roundtrip." + name + "=" + hex(again.marshall()));
                } finally {
                    again.recycle();
                }
            } finally {
                p.recycle();
            }
        }
    }

    private ClientTransactionCompareMain() {
    }
}
