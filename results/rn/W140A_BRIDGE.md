# W140-A — generic `invoke-polymorphic` (`VarHandle` / `MethodHandle`) and the bridge past `ConcurrentLinkedQueue`

Worktree `.cache/wt-w140a` (detached HEAD `4d60477`), built with
`CARGO_PROFILE_DEV_DEBUG=0` through `tools/guarded.sh`. Changes are uncommitted;
copy-back checked with `git apply --check --binary` against the main tree and
then applied. Nothing was committed. Scratch worktree deleted.

## Result at a glance

| stage | W127-A | W140-A |
|---|---|---|
| `runJSBundle` -> `loadScriptFromFile` | abort: `UNSUPPORTED_BYTECODE invoke-polymorphic` at `ConcurrentLinkedQueue$Node.<init>` | **runs** (real `ConcurrentLinkedQueue`/`ConcurrentHashMap`/`Striped64`/`Atomic*FieldUpdater` code) |
| first VM opcode gap | `invoke-polymorphic` | none on this path; next gap is JS-level (`__fbBatchedBridgeConfig` not set before native modules) |
| captured `ACOS_RN_OP` | 0 | still **0** (bridge aborts earlier in the JS bundle bootstrap) |

Raw evidence: `results/rn/w140a-ops.json` (52 stage lines; `ops: []`; trail ends
at `runJSBundle` -> `bridge-error` `com.facebook.jni.CppException: __fbBatchedBridgeConfig
is not set, cannot invoke native modules`).

## 1. Root fix — generic `VarHandle` `invoke-polymorphic`

The VM decoded opcode `0xfa`/`0xfb` but rejected it as
`UNSUPPORTED_BYTECODE (invoke-polymorphic/custom)`. The real core library builds
`VarHandle` objects in guest bytecode and stores the descriptor in the standard
ART fields:

* `MethodHandles.lookup().findVarHandle` -> `java.lang.invoke.FieldVarHandle`
  (`artField:J` = ACOS field offset, `coordinateType0` = declaring class,
  `varType` = value class),
* `findStaticVarHandle` -> `StaticFieldVarHandle` (`artField:J`,
  `declaringClass`),
* `MethodHandles.arrayElementVarHandle` -> `ArrayElementVarHandle`
  (`coordinateType0` = array class, `varType` = component class).

New module `crates/acos-vm/src/natives/varhandle.rs` resolves that descriptor
(`resolve_target`), maps the accessor name to a mode (`get`/`set`/`getAndSet`/
`compareAndSet`/`compareAndExchange`/`getAndAdd`/`getAndBitwise*`, ignoring the
`Acquire`/`Release`/`Opaque`/`Volatile`/`Plain` suffix), and performs the access
on the ACOS heap with type-checked operand conversion (reference handle, int /
sub-word sign-extension, wide long/double). Green threads share one host thread,
so every atomic mode is trivially atomic.

The interpreter hook is deliberately tiny (W136-A owns the perf paths):

* `crates/acos-vm/src/interp.rs`: one `0xfa | 0xfb => invoke(...)` arm in
  `execute`, one fast-path match arm, and a new `invoke_polymorphic` that reads
  the call-site prototype (`insn.index2`) and — like ART — **boxes** a primitive
  result when the proto returns `Object` (D8 emits `move-result-object` +
  `String.valueOf(Object)` for primitive `get`/`getAndAdd`). Unsupported
  receivers/modes return structured `UNSUPPORTED_BYTECODE` naming the receiver
  class and field.
* `crates/acos-vm/src/exec.rs`: `0xfa | 0xfb` lowers to `Op::Invoke`, so the IR
  tier and the general interpreter share the same slow-op path (consistency).
* `crates/acos-vm/src/natives/mod.rs`: `pub(crate) mod varhandle;`.

Per-class shims (`AtomicInteger`/`AtomicLong`/`AtomicBoolean`/`AtomicReference`)
are kept — they are faster than the generic path and already shipped.

`MethodHandle.invoke`/`invokeExact` for `Lookup.findVirtual`/`findStatic`/
`findGetter` is scaffolded through the same receiver-class dispatch but not yet
exercised by a fixture; the `MethodHandle` `artFieldOrMethod`/`handleKind`
descriptor is the next increment. Unsupported `MethodHandle` receivers currently
fail structurally rather than silently.

## 2. Tests

New `crates/acos-vm/tests/varhandle_poly.rs` (+ checked-in
`tests/data/varhandle-concurrency.dex` and its `.java` source) exercises, under
`--corelib real`:

* `findVarHandle` / `findStaticVarHandle` / `arrayElementVarHandle` on
  `int`/`long`/reference fields, volatile and non-volatile,
* `get`/`set`/`getAndSet`/`compareAndSet`/`compareAndExchange`/`getAndAdd`/
  `getAndBitwiseOr` plus `Volatile`/`Acquire`/`Release` variants,
* `ConcurrentLinkedQueue` (the original blocker), `ConcurrentHashMap`,
  `AtomicLong`, `AtomicIntegerFieldUpdater`, `AtomicReferenceFieldUpdater`.

It asserts byte-identical stdout to the host JVM under **all four**
combinations: `{ir, interp}` x `{normal, gc-stress}`.

## 3. Acceptance

| check | result |
|---|---|
| `varhandle_polymorphic_matches_jvm_both_tiers` | **1 passed** (ir/interp x normal/gc-stress) |
| `cargo test -p acos-vm --test dexprogs --test fast_path_equivalence` | **4 passed** |
| `rn_bridge` | **2 passed** (real probe now reaches `runJSBundle`) |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | clean |
| `tools/diff-dexprogs.sh --corelib real` normal | **27 pass / 0 fail / 0 unsupported** |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | **27 pass / 0 fail / 0 unsupported** |

## 4. Files touched

* `crates/acos-vm/src/natives/varhandle.rs` — new; descriptor resolution and
  heap access.
* `crates/acos-vm/src/natives/mod.rs` — module registration.
* `crates/acos-vm/src/interp.rs` — `invoke-polymorphic` dispatch + boxing.
* `crates/acos-vm/src/exec.rs` — IR `Op::Invoke` mapping.
* `crates/acos-vm/tests/varhandle_poly.rs`,
  `crates/acos-vm/tests/data/varhandle-concurrency.{dex,java}` — new tests.
* `crates/acos-jni/tests/rn_bridge.rs` — bridge artifact renamed to
  `results/rn/w140a-ops.json`.
* `results/rn/w140a-ops.json` — new evidence artifact.

No commit.
