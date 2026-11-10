package dev.acos.fixtures.nonvirtual;

/** Base of a two-class hierarchy used by the CallNonvirtual* JNI tests. */
public class Base {
    public String who() {
        return "base";
    }
}
