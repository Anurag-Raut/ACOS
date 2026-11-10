# M2 Status — W68-A (VectorDrawable native tree)

Session: W68-A. Target: `acos launch fixtures/build/apps/counter.apk`
(`ACOS_BOOT_DIR=.cache/boot/android-34`).

## Bottom line

`VectorDrawable` native tree is **implemented and registered**. The launch now
gets past the W65-A blocker (`VectorDrawable.nCreateGroup()J` unimplemented) and
reaches `client_transaction`, but **`activity_on_create` now fails with a
NullPointerException** in the framework's `ResourcesImpl.loadDrawableForCookie`.
This is a **resource/TypedArray path**, not a VectorDrawable path — the
VectorDrawable natives are being called and succeed.

## What is done

| Item | Status | Evidence |
|---|---|---|
| `crates/acos-vm/src/natives/gfx_vector.rs` (new) | done, 1554 lines | native tree + render into `acos-render` display list |
| Registration in `crates/acos-vm/src/natives/mod.rs` | done | `pub mod gfx_vector;` line 33, `gfx_vector::install(vm)?;` line 110 |
| Tree natives: `nCreateTree`, `nCreateTreeFromCopy`, `nCreateFullPath`(±copy), `nCreateClipPath`(±copy), `nCreateGroup`(±copy), `nAddChild`, `nSetName` | done | registered lines 249–257 |
| Viewport/alpha/caching/draw: `nSetRendererViewportSize`, `nSetRootAlpha`, `nGetRootAlpha`, `nSetAntiAlias`, `nSetAllowCaching`, `nDraw` | done | registered lines 260–266; `nDraw` renders via `gfx_canvas::push_op` + `DisplayOp::Save/Concat/ClipPath/Restore` |
| Property packing: `nGetFullPathProperties` (12×32-bit slots), `nGetGroupProperties` (7×f32), `nUpdateFullPathProperties`, `nUpdateFullPathFillGradient`, `nUpdateFullPathStrokeGradient`, `nUpdateGroupProperties` | done | registered lines 268–274 |
| Path/group setters+getters (stroke width/color/alpha, fill color/alpha, trim start/end/offset, rotation, pivot, scale, translate) | done | registered lines 277–306 |
| `android.util.PathParser` peers (`nCreateEmptyPathData`, `nCreatePathDataFromString`, `nCreatePathData`, `nCreatePathFromPathData`, `nParseStringForPath`, `nSetPathData`, `nFinalize`, `nCanMorph`, `nInterpolatePathData`) | done | registered lines 309–320 |
| Path parser semantics (`PathParser` grammar, relative commands, arcs) | done | unit tests `parses_all_command_letters`, `parses_the_ic_ab_back_material_path`, `parses_the_progress_bar_arc_path` |
| Reference-diff tests vs ART PNG | **not done** | needs ART oracle capture; see "Next" |
| `cargo check --workspace --all-targets` | **green** | 6.61 s, `Finished dev profile` |
| `cargo test -p acos-vm --lib gfx_vector` | **5/5 pass** | `test result: ok. 5 passed; 0 failed` |
| `tools/diff-dexprogs.sh --corelib real` | **27/27 pass / 0 fail / 0 unsupported** | `results/vm/dexprogs-real-normal.json` |

Debug `eprintln!` instrumentation left by the prior run was removed from
`gfx_vector.rs` before the final check.

## Exact current blocker

Stage: **`activity_on_create`** (`performLaunchActivity` throws before
`Activity.onCreate`). Reached stages: `apk_parsed`, `process_booted`,
`bind_application`, `application_on_create`, `client_transaction`.

Exception chain (from `acos launch --json`):

```
java.lang.RuntimeException: Unable to start activity ... MainActivity:
  android.content.res.Resources$NotFoundException:
      Drawable android:drawable/ic_ab_back_material with resource ID #0x108031e
    at android.content.res.ResourcesImpl.loadDrawable(733)
  caused by: Resources$NotFoundException:
      File res/drawable/ic_ab_back_material.xml from drawable resource ID #0x108031e
    at android.content.res.ResourcesImpl.loadDrawableForCookie(915)
  caused by: java.lang.NullPointerException
    at android.content.res.ResourcesImpl.loadDrawableForCookie(909)
```

Bytecode facts (from `dexdump` of
`.cache/boot/android-34/dex/framework/classes.dex`, method
`ResourcesImpl.loadDrawableForCookie:(Resources;TypedValue;II)Drawable`):

* line 909 maps to `0x00cf`/`0x00d9` — the **`finally { stack.pop(); }`** block
  (`invoke-virtual {v14}, ResourcesImpl$LookupStack;.pop()` at `0x00d6`).
* line 915 is the `throw NotFoundException("File " + file + " ...")` in the
  `catch`.
* So the NPE is thrown in the `finally`, i.e. **`v14` (the `LookupStack`
  returned by `mLookupStack.get()`) is null** (or `pop()` dereferences null).
  `mLookupStack` is a `ThreadLocal<LookupStack>`; `get()` should fall back to
  `initialValue()` (`lambda$new$0` → `new LookupStack()`), so the VM's
  `ThreadLocal.get()`/`ThreadLocalMap` path is returning null instead of the
  initial value.

Call site: `DecorView.onResourcesLoaded` → `screen_toolbar` →
`ToolbarWidgetWrapper.<init>` → `TypedArray.getDrawable` →
`Resources.loadDrawable` → `ResourcesImpl.loadDrawable` →
`loadDrawableForCookie`.

The native trace confirms the VectorDrawable natives are invoked in order
(`nCreateGroup`, `nCreateTree`, `nSetRendererViewportSize`, `nGetRootAlpha`,
`nSetRootAlpha`, `nCreateFullPath`, `nGetFullPathProperties`,
`PathParser.nCreatePathDataFromString`, `nSetPathString`) and **no longer
report "not implemented"** — the W65-A blocker is resolved.

## What was tried

1. Read `results/m2/M2_LOG.md` W51-A/W60-A/W65-A sections and
   `docs/DEV_WORKFLOW.md`.
2. Confirmed `gfx_vector.rs` is registered and the release binary was rebuilt
   from the current source (mtime 02:03 vs source 02:03).
3. Ran `acos launch fixtures/build/apps/counter.apk`; captured the full JSON
   exception chain with `--json`.
4. Decompiled the real framework DEX (`dexdump`) to map the stack line
   `loadDrawableForCookie(909)` to the `finally { stack.pop(); }` block and
   confirm the null is the `LookupStack`.
5. Verified `cargo check --workspace --all-targets` green and
   `diff-dexprogs --corelib real` 27/27 pass.
6. Removed debug `[DBG]` prints from `gfx_vector.rs`.

## Next steps for a fresh agent

1. **Fix the NPE**: trace `java.lang.ThreadLocal.get()` /
   `ThreadLocal$ThreadLocalMap.getEntry()` / `ThreadLocal.setInitialValue()` in
   the VM's `core-oj` execution. The `LookupStack` initial value
   (`ResourcesImpl.lambda$new$0`) must be created; the null indicates
   `ThreadLocal.get()` is returning null. Check whether `Thread.threadLocals` is
   initialised and whether `createMap()` runs. This is the `acos-services`/
   `acos-runtime` resource path (W65-A's `Configuration` suspect is downstream
   of this).
2. After the NPE clears, the next expected blockers from W65-A's DEX-diff
   survey: `android.content.res.Configuration` in the `addToDisplayAsUser`
   reply (0/20 vs 27 fields in `crates/acos-services/src/parcelable.rs`; use
   `FieldOp::Cond` or materialise real Java `Configuration` via javabridge),
   then `SizeConfigurationBuckets`, then `ClientTransaction`.
3. Un-ignore/extend `crates/acos-runtime/tests/m2.rs` to assert `on_resume`
   reached; update the ignore reason.
4. **Reference-diff tests** for VectorDrawable: create a tiny dexprog/fixture
   with a known VectorDrawable XML, capture reference PNG from the ART oracle
   (`tools/oracle`, API-34 headless emulator, one instance, ≤2048 MB guest, only
   when `MemAvailable > 16 GiB`, always shut down) or `fixtures/reference-device`,
   and assert `acos` frame-diff SSIM > 0.98. The natives already render into the
   display list; this needs the oracle capture and the `nDraw` output plumbing.

## Files touched

* `crates/acos-vm/src/natives/gfx_vector.rs` (new, 1554 lines)
* `crates/acos-vm/src/natives/mod.rs` (2 lines: `pub mod gfx_vector;`,
  `gfx_vector::install(vm)?;`)
* `results/M2_STATUS_W68A.md` (this file)

No commits made.
