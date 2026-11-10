import android.content.ClipData;
import android.content.ClipDescription;
import android.content.IClipboard;
import android.content.IOnPrimaryClipChangedListener;
import android.os.IBinder;
import android.os.ServiceManager;
import android.view.inputmethod.EditorInfo;
import com.android.internal.inputmethod.InputBindResult;
import com.android.internal.view.IInputMethodManager;

/**
 * T-38 follow-up acceptance at the Java level: the real framework
 * {@code IInputMethodManager$Stub$Proxy} and {@code IClipboard$Stub$Proxy} read
 * the ACOS replies (materialised by {@code acos-runtime::ime_java} /
 * {@code clipboard_java}) over the real Binder transport.
 *
 * <p>Runs on the real Android 14 core library + framework DEX. The ACOS
 * services are the in-process Rust state machines; the parcel bytes are written
 * by the real framework objects' own {@code writeToParcel}.
 *
 * <p>The null in-process IME (no {@code InputMethodService}) must answer
 * {@code startInputOrWindowGainedFocus} with {@code InputBindResult.result ==
 * NO_IME(1)} and never hang. The clipboard must re-emit exactly the
 * {@code ClipData} that was set, and deliver
 * {@code IOnPrimaryClipChangedListener.onPrimaryClipChanged()} once per change.
 */
public final class ImeClipboardMain {
    static int clipChanged;

    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    static void out(String key, Object value) {
        System.out.println(key + "=" + value);
    }

    public static void main(String[] args) throws Exception {
        // ---- IInputMethodManager: the null IME ----------------------------
        IBinder immBinder = ServiceManager.getService("input_method");
        check(immBinder != null, "getService(input_method) must return a binder");
        IInputMethodManager imm = IInputMethodManager.Stub.asInterface(immBinder);
        check(imm != null, "asInterface(input_method) must produce a proxy");

        imm.addClient(null, null, 0);
        out("addClient", "ok");
        check(imm.getInputMethodList(0, 0).isEmpty(), "no IME installed for the user");
        check(imm.getEnabledInputMethodList(0).isEmpty(), "no enabled IME");
        check(imm.getCurrentInputMethodSubtype(0) == null, "no current IME subtype");
        out("imeList", "empty");

        EditorInfo attribute = new EditorInfo();
        attribute.inputType = 1;
        attribute.imeOptions = 6;
        attribute.packageName = "dev.acos.fixtures.helloviews";
        InputBindResult bind = imm.startInputOrWindowGainedFocus(
                0, /* startInputReason */
                null, /* IInputMethodClient */
                null, /* windowToken */
                0, /* controlFlags */
                0, /* softInputMode */
                0, /* windowFlags */
                attribute,
                null, /* IRemoteInputConnection */
                null, /* IRemoteAccessibilityInputConnection */
                1, /* unverifiedTargetSdkVersion */
                0, /* userId */
                null /* ImeOnBackInvokedDispatcher */);
        check(bind != null, "startInput must return a non-null InputBindResult");
        out("bindResult", bind.result);
        out("noImeResult", InputBindResult.NO_IME.result);
        check(bind.result == InputBindResult.NO_IME.result,
                "the null IME must report NO_IME(" + InputBindResult.NO_IME.result
                        + "), got " + bind.result);
        System.out.println("ime-bind-ok");

        // ---- IClipboard: set/get + listener delivery ----------------------
        IBinder clipBinder = ServiceManager.getService("clipboard");
        check(clipBinder != null, "getService(clipboard) must return a binder");
        IClipboard clipboard = IClipboard.Stub.asInterface(clipBinder);
        check(clipboard != null, "asInterface(clipboard) must produce a proxy");

        final String pkg = "dev.acos.fixtures.helloviews";
        IOnPrimaryClipChangedListener listener = new IOnPrimaryClipChangedListener.Stub() {
            @Override
            public void dispatchPrimaryClipChanged() {
                clipChanged++;
            }
        };
        clipboard.addPrimaryClipChangedListener(listener, pkg, null, 0, 0);
        out("addListener", "ok");

        ClipData clip = ClipData.newPlainText("l", "hello");
        clipboard.setPrimaryClip(clip, pkg, null, 0, 0);
        out("hasPrimaryClip", clipboard.hasPrimaryClip(pkg, null, 0, 0));
        check(clipboard.hasPrimaryClip(pkg, null, 0, 0), "hasPrimaryClip after set");

        ClipData back = clipboard.getPrimaryClip(pkg, null, 0, 0);
        check(back != null, "getPrimaryClip must not be null");
        check(back.getItemCount() == 1, "clip item count");
        check("hello".equals(back.getItemAt(0).getText().toString()),
                "clip text: " + back.getItemAt(0).getText());
        out("clipText", back.getItemAt(0).getText());

        ClipDescription description = clipboard.getPrimaryClipDescription(pkg, null, 0, 0);
        check(description != null, "getPrimaryClipDescription must not be null");
        check(description.getLabel().toString().equals("l"),
                "description label: " + description.getLabel());
        check(description.hasMimeType("text/plain"), "text/plain MIME type");
        out("clipLabel", description.getLabel());

        // One listener delivery per change.
        out("clipChanged", clipChanged);
        check(clipChanged == 1, "listener must fire exactly once per change, got " + clipChanged);

        clipboard.setPrimaryClip(ClipData.newPlainText("l", "world"), pkg, null, 0, 0);
        check(clipChanged == 2, "listener must fire once per change, got " + clipChanged);
        check("world".equals(clipboard.getPrimaryClip(pkg, null, 0, 0)
                .getItemAt(0).getText().toString()), "second clip text");

        clipboard.clearPrimaryClip(pkg, null, 0, 0);
        check(!clipboard.hasPrimaryClip(pkg, null, 0, 0), "cleared");
        check(clipChanged == 3, "clear is a change, got " + clipChanged);
        System.out.println("clipboard-ok");

        // ---- IUserManager: the real UserInfo parcelable --------------------
        IBinder userBinder = ServiceManager.getService("user");
        check(userBinder != null, "getService(user) must return a binder");
        android.os.IUserManager users = android.os.IUserManager.Stub.asInterface(userBinder);
        check(users != null, "asInterface(user) must produce a proxy");
        android.content.pm.UserInfo user = users.getUserInfo(0);
        check(user != null, "getUserInfo(0) must not be null");
        check(user.id == 0, "user id: " + user.id);
        check("ACOS".equals(user.name), "user name: " + user.name);
        check(users.getUserInfo(7) == null, "getUserInfo(7) must be null");
        java.util.List all = users.getUsers(false, false, false);
        check(all.size() == 1, "exactly one ACOS user, got " + all.size());
        out("user", user.id + "/" + user.name);
        System.out.println("user-ok");

        System.out.println("ime-clipboard-ok");
    }
}
