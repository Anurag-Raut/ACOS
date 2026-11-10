# ACOS ambient-state probes (T-45 / T-46)

Static probes that close the argument-value gaps the dynamic ART traces cannot
capture (report gaps **G-2** system-property keys and **G-3** system-feature
names).  Both are deterministic, stdlib-only Python and read the DEX
disassembly through the in-repo `acos-dex` example:

```bash
cargo run -p acos-dex --example dexdump -- <file.dex>
```

`tools/probes/acos_ambient.py` is the shared engine: it parses the
`dexdump` text form, follows a small register flow (`const-string` / `const` /
`move` / `sget` / `sput` / `new-instance` / `StringBuilder` concat / `invoke` /
`if`) and resolves non-inlined `static final` fields from every DEX `<clinit>`.

## T-45 — system-property keys

```bash
python3 -I tools/probes/prop-keys.py            # full run
python3 -I tools/probes/prop-keys.py --selftest # known-answer fixture
```

Scope: every method in the executed-method sets
(`research/dynamic-trace/data/methods_{println,hello_views_launch,hello_views_interaction}.tsv`)
plus `Build.<clinit>` / `Build$VERSION.<clinit>` and the `android.sysprop.*`
accessors that are reachable from those methods.  For each
`SystemProperties.{get,getInt,getLong,getBoolean,find,native_get*,addChangeCallback}`
call site it reports the key const-string, the caller default and the phase(s)
in which the calling method executes.

Output: `research/ambient-state/sysprops.tsv` (`key`, `default_arg`,
`accessor_kind`, `calling_methods`, `executed_in`, `value_to_supply`, `status`
(`known`/`gap`/`dynamic`/`n/a`), `dynamic`, `origin`
(`executed`/`supplementary`/`sysprop`), `build_field`, `note`).  Values come
from `.cache/boot/android-34/build.prop`; missing keys are `gap` with a
documented AOSP recommended default.

## T-46 — feature / service / settings / thread / SDK-gate audit

```bash
python3 -I tools/probes/feature-audit.py            # full run
python3 -I tools/probes/feature-audit.py --selftest # known-answer fixture
```

Scope: the same executed-method sets plus `SystemServiceRegistry.<clinit>` and
`Build.<clinit>` / `Build$VERSION.<clinit>`.

Outputs under `research/ambient-state/`:

| file | contents |
|---|---|
| `features.tsv` | `hasSystemFeature` names, `SystemConfig`/`FeatureInfo`, `DeviceConfig` / `*Flags`, `Configuration`/`DisplayMetrics`/`LocaleList` reads |
| `services_requested.tsv` | `Context.getSystemService(String)` / `getSystemServiceName` constants and every `SystemServiceRegistry.registerService` name |
| `settings_keys.tsv` | `Settings.{Global,Secure,System}` get* keys + caller defaults + literal/field provenance |
| `thread_names.tsv` | const-string thread names passed to `HandlerThread` / `Thread` constructors |
| `sdk_gates.tsv` | `Build.VERSION.*` / `Build.*` reads and branch comparisons (field, site, compared value, value to serve) |
| `REPORT.md` | counts, the exact values ACOS should return, and the references' GAPs with the exact `adb` command to capture each |

## Self-tests

`--selftest` builds `tools/probes/selftest/` with `javac` + `d8` into
`.cache/probes/selftest/classes.dex` (the same toolchain as
`tools/build-fixtures.sh`; the only hidden-API stub is
`selftest/stub/android/os/SystemProperties.java`) and asserts the known keys,
defaults, feature names, settings keys, thread name and SDK gate.

## Options

Both tools accept `--dex FILE` (repeatable; overrides the default DEX set),
`--boot`, `--trace`, `--all-methods`, `--quiet`; `prop-keys` also `--out`;
`feature-audit` also `--out-dir` and `--reference`.  Set `$ACOS_DEXDUMP` to
override the disassembler command.
