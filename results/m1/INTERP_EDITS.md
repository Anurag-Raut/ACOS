# `interp.rs` edits — merge list for `vm-perf`

`crates/acos-vm/src/interp.rs` is being rewritten concurrently on the `vm-perf`
branch. Everything I could move out is now in `src/bridge.rs` (new),
`src/natives/` (new), `vm.rs` and `heap.rs`; this file is the **complete** list
of what is left in `interp.rs` so the merge is mechanical.

Base: `git diff --stat crates/acos-vm/src/interp.rs` is now **+74/−68**
(down from +244/−48 before the extraction). Only the hunks below remain.

---

## 1. `run()` — thread teardown → `bridge::finish_thread`

```rust
// removed (now in bridge.rs):
//   fn finish_thread(vm: &mut Vm, t: usize) { ... }
//   fn debug_threads(vm: &Vm) { ... }
crate::bridge::finish_thread(vm, t);
```

`finish_thread` marks the thread finished, clears `nativePeer`/`tid`, wakes
`Thread.join` waiters and `Object.wait`ers on the thread's monitor, and calls
`heap` finalisers. Pure VM bookkeeping; no interpreter state.

## 2. `step()` — native dispatch → `bridge::record_native`

```rust
crate::bridge::record_native(vm, method);   // --trace-natives bookkeeping
```

One call, immediately before a native is invoked. Moves the whole
`NativeTrace`/`first_order`/`call_site` implementation into `bridge.rs`.

## 3. `step()` — unbound native error → `bridge::unbound_native_error`

```rust
if vm.methods[method as usize].is_native() {
    let at = format!("{} pc={}", vm.frames_label(t, fi), vm.threads[t].frames[fi].pc);
    return Err(crate::bridge::unbound_native_error(vm, method, at));
}
```

Produces the structured `UNSUPPORTED_JAVA_API` / `Lclass;->name(sig)` error.
Also caches `desc`/`name`/`sig` for the trace call in (2).

## 4. `execute()` — `iget`/`iput` shadow fields → `bridge::internal_field_get/put`

```rust
// iget / iget-object / iget-wide
if let Some(v) = crate::bridge::internal_field_get(vm, obj, dex, insn.index) {
    ...
} else if f.width == 2 { ... }

// iput / iput-object / iput-wide
if crate::bridge::internal_field_put(vm, obj, dex, insn.index) {
    // Shadow String fields (`hash`) cached in a side table.
} else if f.width == 2 { ... }
```

`internal_field_get` synthesises the DEX-visible fields of the VM-internal
classes: `String.count/hash/coder/value`, `Class.accessFlags/primitiveType/
componentType/superClass`, `Object.shadow$_klass_/shadow$_monitor_`.
`internal_field_put` handles the `String.hash` writeback. This used to be
~105 lines of `string_field_get/put` inside `interp.rs`.

**Contract for the merger:** the hooks must be tried *before* the normal
instance-field read/write, and a `Some`/`true` result means the hook handled it
(the normal path must be skipped).

## 5. `arith3` / `arith2addr` — long shift operand width (interpreter bug)

`shl-long`/`shr-long`/`ushr-long` (opcodes `0xa3..=0xa5` and `0xc3..=0xc5`)
take an **int** shift amount (`vCC` / `vB`), not a wide one. The old code read a
wide register, which panicked/miscompiled `Long.rotateLeft`, `01<<n`, etc.

```rust
let v: i64 = if (0xa3..=0xa5).contains(&insn.op) {
    let sh = (reg(vm, t, fi, r2) as i32 & 0x3f) as u32;
    match insn.op { 0xa3 => (a as u64).wrapping_shl(sh) as i64, ... }
} else {
    let b = get_wide(vm, t, fi, r2) as i64;  // the other long ops are wide
    ...
}
```

Same shape in `arith2addr` with `0xc3..=0xc5`.

## 6. `execute()` — `<clinit>` counter

```rust
self.note_clinit();   // Metrics.clinit_count, for the M1 startup table
```

One call in the `<clinit>` invocation path.

## 7. `get_wide` bounds guard

`get_wide` now returns 0 instead of panicking when `vB`/`vCC` runs past the
frame (the verifier already guarantees this, but the long-shift path stressed
it). One `if` at the top of `get_wide`.

---

## What was extracted (for reference)

| removed from `interp.rs` | now in |
|---|---|
| `string_field_get` / `string_field_put` (~105 lines) | `bridge::internal_field_get/put` |
| `finish_thread`, `debug_threads` | `bridge::finish_thread` |
| native trace recording + unbound-native error text | `bridge::{record_native,unbound_native_error}` |
| all the M1 native bodies, `String` content overrides, `Thread`, `System`, `ICU`, `Linux`, `Unsafe`, `Class`/`Field`/`Array`, `Math` | `src/natives/*.rs` (registry) |
| real-vs-bootstrap class provider, boot-classpath loading, native registry, thread/monitor setup | `vm.rs` |
| shadow-field storage for internal classes | `heap.rs`/`vm.rs` |

Net effect: `interp.rs` keeps **only call-sites**, so `vm-perf`'s rewrite of the
loop can be rebased by re-applying hunks 1–7 (all tiny).
