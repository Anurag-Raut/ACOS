package android.content;

import android.os.Bundle;
import android.os.IBinder;
import android.os.ServiceManager;
import android.util.MemoryIntArray;

/**
 * T-27 acceptance: the Settings provider (`IContentProvider.call`) is driven
 * through the real hand-written {@code ContentProviderProxy}, the generation
 * counter is a real fd-backed {@code MemoryIntArray}, and a PUT bumps the
 * generation the client observes.
 *
 * <p>The class lives in package {@code android.content} because
 * {@code ContentProviderProxy} is package-private; the real class is loaded
 * from the boot classpath at runtime.
 */
public final class SettingsMain {
    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    static void out(String key, Object value) {
        System.out.println(key + "=" + (value == null ? "<null>" : value));
    }

    static Bundle call(IContentProvider provider, AttributionSource source,
            String method, String key, Bundle extras) throws Exception {
        Bundle b = provider.call(source, "settings", method, key, extras);
        check(b != null, method + " returns a Bundle");
        return b;
    }

    public static void main(String[] args) throws Exception {
        IBinder binder = ServiceManager.getService("settings");
        check(binder != null, "getService(settings) must return the provider binder");
        IContentProvider provider = new ContentProviderProxy(binder);
        AttributionSource source = AttributionSource.myAttributionSource();

        // `Settings.Global.getInt(cr, "animator_duration_scale", 1)`: the key is
        // unset on the reference image, so the caller's default (1) applies.
        Bundle b = call(provider, source, "GET_global", "animator_duration_scale", null);
        String raw = b.getString("value");
        int value = raw == null ? 1 : Integer.parseInt(raw);
        out("settings.animator_duration_scale", value);
        out("settings.generation.initial", b.getInt("generation_int"));
        MemoryIntArray mia = b.getParcelable("generation", MemoryIntArray.class);
        // Do not print the object identity (it embeds a VM handle); print the
        // value the real `MemoryIntArray` exposes over its fd.
        out("settings.generation.array.present", mia != null);
        if (mia != null) {
            out("settings.generation.array.value", mia.get(0));
        }

        // A PUT bumps the generation the client observes.
        Bundle put = new Bundle();
        put.putString("value", "0");
        call(provider, source, "PUT_global", "animator_duration_scale", put);

        Bundle after = call(provider, source, "GET_global", "animator_duration_scale", null);
        out("settings.animator_duration_scale.after_put", after.getString("value"));
        out("settings.generation.after_put", after.getInt("generation_int"));
        // The PUT bumps the counter in the fd the client already holds, so the
        // same `MemoryIntArray` must observe the new generation.
        if (mia != null) {
            out("settings.generation.array.after_put", mia.get(0));
        }

        // Seeded `Settings.System` keys and an unset Secure key.
        Bundle sys = call(provider, source, "GET_system", "auto_caps", null);
        out("settings.auto_caps", sys.getString("value"));
        Bundle secure = call(provider, source, "GET_secure", "definitely_unset_key", null);
        out("settings.secure_unset", secure.getString("value"));

        System.out.println("settings=ok");
    }
}
