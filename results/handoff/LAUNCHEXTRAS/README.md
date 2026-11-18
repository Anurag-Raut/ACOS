# LAUNCHEXTRAS — the launch `Intent` (action, data URI, typed extras) reaches `getIntent()`

Branch `launchextras` (base `scenhyg` @ `8b3afbdd`), resumed from the WIP commit
`6c69d69a` (host power-off). Worktree
`/home/Qui/.t3/worktrees/acos/t3-2d2bb735-60949492e8cb8af5`; target
`.cache/launchextras-target`; private `ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET`
(`.cache/rt-lx`), `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`,
`ACOS_TEST_ARTIFACTS=$PWD/.cache/test-artifacts`; every ART run through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`.

SCENHYG §4 refused a scenario's `intent_action:`, `data:` and `extras:` with
`UNSUPPORTED_ANDROID_API` because the live launcher built its own
`ACTION_MAIN`/`CATEGORY_LAUNCHER` Intent. Those three now travel through the
host-built session-start `LaunchActivityItem` (ADR-0029 note in `DECISIONS.md`),
so the Activity's real `getIntent()` is the declared Intent. Only a `Parcelable`
extra (and a launch step that asks for a different Intent than the live session
already delivered) stays refused, **by name**.

## 1. What was already in the WIP commit, and what this resume added

The WIP commit `6c69d69a` had the whole feature at the wire level (protocol
`ExtraValue`/`LaunchIntent`, the Intent/Uri codec, `acos-art`/CLI/MCP plumbing,
the scenarios, the real-Java differential and the golden). It was **not
verified end to end, and it was wrong on one axis**:

**The defect (found by running the scenario, not by reading it).** The launch
step failed at `assert_log`; logcat showed

```
W Bundle: Key probe_float expected Float but value was a java.lang.Double.
          The default value 0.0 was returned.
java.lang.ClassCastException: java.lang.Double cannot be cast to java.lang.Float
```

Root cause: `ExtraValue` crossed a **JSON** hop on every surface (scenario →
engine → `ACOSLAUNCH_INTENT` → the launcher; `acos launch`/`acos_launch` → the
daemon) and the writer emitted a `long`/`float` as a bare JSON number, which the
reader can only infer back as `int`/`double`. That is a different Java type and
a different `Bundle` tag (`VAL_INTEGER` vs `VAL_LONG`, `java.lang.Double` vs
`java.lang.Float`).

Fix, in the one place that owns the type tag (`crates/acos-protocol/src/launch.rs`):
`ExtraValue::Serialize` writes the explicit `{type, value}` form for `long`,
`float` and `parcelable`; the unambiguous scalars stay plain. Teeth:
`json-wire-before.txt` / `json-wire-after.txt` (the new protocol test fails
before, passes after: `{"extras":{"count":7,"factor":1.5,"ratio":1.5,"small_long":7}}`
→ `ratio: Double(1.5)`, `small_long: Int(7)`).

Also on this resume:

* `scenarios/launch-extras.yaml` gained `probe_long_small: {type: long, value: 7}`
  (a `long` that fits an `int` — the exact ambiguity class) and the probe logs
  it, so the end-to-end run pins it; the golden, the real-Java case and the
  differential carry the same extra (golden 632 → 684 bytes, re-derived **after**
  the differential passed).
* one `--extra KEY=VALUE` parser (`ExtraValue::parse_key_value`), used by both
  `acos launch` and `acos-art launch` (they had two literal copies).
* unit tests for the daemon argument reader (`launch_intent_arg`) and the CLI
  parser, plus the protocol wire test.
* `DECISIONS.md` ADR-0029 note (the Intent is host-built; the type tag is part
  of the value; what stays refused) and the stale `UnsolicitedParcelable(android.net.Uri)`
  row in `docs/IMAGE_ABI_NOTEBOOK.md` corrected to the measured StringUri shape.

## 2. How the Intent is carried (one paragraph, for review)

`acos_services::client_transaction::encode_launch_intent` writes
`Intent.writeToParcel` bytes exactly as this image does — action `String8`,
`mData` as `Uri`: `writeInt(0)` null / `writeInt(type)` + `writeString8(toString())`
for the three non-null types (this image's `Uri.parse` always builds a
`StringUri`, tag 1), the component as the static writer, categories, flags
`FLAG_ACTIVITY_NEW_TASK`, extras as a real `Bundle` (`BUNDLE_MAGIC`, ArrayMap
order, one typed `VAL_*` per extra). `acos-art` passes the hex in
`ACOSLAUNCH_INTENT_BYTES`; the guest's `LaunchMain.hostLaunchIntent()`
unmarshals it with the framework's own `Intent.CREATOR` and hands that Intent to
the real `LaunchActivityItem`. Nothing is rebuilt guest-side from loose fields.
With no declared Intent the guest builds the stock MAIN/LAUNCHER Intent exactly
as before (byte-identical: the env var is simply absent).

## 3. Acceptance evidence

| item | file | result |
|---|---|---|
| `cargo test -p acos-protocol -p acos-verify -p acos-mcp` | `suite-1.log` | 31 targets, **455 passed, 0 failed** |
| `cargo test -p acos-services` | `suite-2-services.log` | 75 targets, **628 passed, 0 failed** |
| `cargo test -p acos-art -p acos-cli` | `suite-3-art-cli.log` | 42 targets, **548 passed, 0 failed** |
| `cargo test -p acos-runtime --test client_transaction_java` (the real-Java differential: forward bytes + reverse `CREATOR` + re-emit, `launch_intent_extras` included) | `real-java-differential.log` | `1 passed` |
| `scenarios/launch-extras.yaml` on ART 34 | `scenarios/launch-extras/{run.json,verdict.json,logcat.txt,probe-lines.txt,provenance.txt}` | **pass**, every step |
| `scenarios/launch-extras-parcelable.yaml` on ART 34 | `scenarios/launch-extras-parcelable/…` | **unsupported** + `UNSUPPORTED_ANDROID_API` naming `device` / `android.os.Bundle` |
| `scenarios/counter.yaml` | `scenarios/counter/…` | **pass** |
| `scenarios/form-validation.yaml` | `scenarios/form-validation/…` | **pass** |
| `scenarios/gaten-backstack.yaml` | `scenarios/gaten-backstack/…` | **pass** |
| `scenarios/mproc3-two-process.yaml` | `scenarios/mproc3-two-process/…` | **pass** |
| `acos launch --engine art --keep --intent-action … --data … --extra …` (CLI/daemon surface) | `cli-launch/{launch.json,logs.json,probe-lines.txt}` | `ok: true`, guest logs the action, data and every typed extra |
| the JSON-wire fix, before/after | `json-wire-before.txt`, `json-wire-after.txt` | fails before, passes after |

The probe's own `getIntent()` reading (`probe-lines.txt`, both the scenario and
the CLI run):

```
action=dev.acos.fixtures.launchextras.ACTION
data=acos://launch/extras?source=scenario   (CLI run: ?source=cli)
string=hello
int=42
long=5000000000
long_small=7
boolean=true
float=1.5
double=0.25
string_array=[alpha, beta]
```

`float=1.5` is the row that was `0.0` before the JSON-wire fix; `long_small=7`
is the same class (`getLong` on an `int` reads 0).

## 4. What is refused, and why it is honest

* **A `Parcelable` extra** — `UNSUPPORTED_ANDROID_API` naming the key and class,
  before any guest starts (it needs the app's own class loader and a payload a
  scenario cannot express). Refusing is honest; dropping it is the WVFAIR bug.
* **A launch step that declares a different Intent than the live session started
  with** — `INVALID_ARGUMENT`: one guest process receives one first Intent.
  `resolve_launch_intent` refuses two disagreeing launch steps in one scenario
  (`INVALID_SCENARIO`) before a guest exists.

`activity` is deliberately not an Intent field: it is the executor's
manifest-existence check (20 gaten scenarios name `MainActivity` while the
manifest launcher entry is a splash/router alias).

## 5. Notes / decisions the orchestrator may want to see

* Scenario validation no longer refuses `activity` **and** `intent_action`
  together: they are independent fields now (component vs Intent action), which
  is exactly what this transport carries. No existing scenario used
  `intent_action`, so nothing else changes.
* The golden (`crates/acos-services/tests/fixtures/intent-launch-extras.bin`) is
  refreshed only **after** the real-Java differential passes; the refresh command
  is documented in `intent_launch_golden.rs` (`UPDATE_INTENT_GOLDEN=1`).
* Scratch deleted at the end: `.cache/launchextras-target`, the `cp -al`
  `fixtures/build` copy, `.cache/lx-artifacts`, `.cache/rt-lx`, `.cache/lv`,
  `.cache/test-artifacts`, `.cache/*.log`, `.cache/*.sh`. The committed logs
  and JSON/PNG evidence are what remains.
