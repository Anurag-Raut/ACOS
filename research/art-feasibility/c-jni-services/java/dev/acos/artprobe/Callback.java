package dev.acos.artprobe;

import java.util.ArrayList;
import java.util.List;

/** Static callback target, called back from native code. */
public final class Callback {
    public static final List<String> events = new ArrayList<>();

    public static void onNativeEvent(String msg) {
        events.add(msg);
    }
}
