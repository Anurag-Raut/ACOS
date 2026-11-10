package dev.acos.fixtures.nonvirtual;

/** Overrides {@link Base#who()} so virtual and non-virtual dispatch differ. */
public class Derived extends Base {
    @Override
    public String who() {
        return "derived";
    }
}
