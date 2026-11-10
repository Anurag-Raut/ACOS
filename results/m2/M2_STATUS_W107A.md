# M2 Status — W107-A (`TypedArray` stale-slot leak; NPE cleared)

Session: W107-A. Target: `acos launch fixtures/build/apps/counter.apk`
(`ACOS_BOOT_DIR=.cache/boot/android-34`). Worktree HEAD `6409190`,
`CARGO_TARGET_DIR=.cache/target-w107a`, `CARGO_PROFILE_DEV_DEBUG=0`. No commits.

## Bottom line

The W68-A blocker (`NullPointerException` in
`ResourcesImpl.loadDrawableForCookie`) is **fixed**. It was not a ThreadLocal
bug: a pooled `TypedArray`'s stale `mData` slot leaked the parent `<vector>`'s
`android:width` into `VectorDrawablePath_fillColor`, so
`ResourcesImpl.loadComplexColor` dereferenced a null `TypedValue.string`. The
launch now gets past `DecorView.onResourcesLoaded` and
`android:drawable/ic_ab_back_material` inflates.

The launch now blocks one step further, in `VectorDrawable.applyTheme`
resolving the vector's tint complex color:
`UnsupportedOperationException: Can't convert to ComplexColor: type=0x1` at
`ResourcesImpl.loadComplexColorForCookie`.

## What is done

| Item | Status | Evidence |
|---|---|---|
| Root-cause instrumentation (temporary) | done, removed | `handle_exception_inner` showed `v14` non-null; `Vm::capture_stack` showed `loadComplexColor(1057)` as the innermost NPE |
| Fix `AssetManager::retrieve_attributes` clears absent `mData` slots | done | `crates/acos-framework-natives/src/res/theme.rs` |
| Regression test | done | `xmlblock_diff::retrieve_attributes_clears_stale_slots_from_a_recycled_typed_array` |
| ThreadLocal minimal repro (`--corelib real`, IR + interp, ±`ACOS_GC_STRESS`) | passes, no VM bug | `results/m2/w107a-threadlocal-repro/` |
| `cargo test -p acos-framework-natives` | green | xmlblock_diff 5/5 |
| `cargo test -p acos-vm --lib` | green | 120/120 |
| `cargo test -p acos-runtime --test m2` | green | 2 passed / 1 ignored |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | green | +6 pre-existing `gfx_vector.rs` lints fixed |
| `cargo check --workspace --all-targets` | green | |
| `diff-dexprogs.sh --corelib real` (normal) | 27/27 | |
| `diff-dexprogs.sh --corelib real --gc-stress` | 27/27 | |

## Exact current blocker

`activity_on_create`, inside `VectorDrawable.applyTheme`:

```
java.lang.UnsupportedOperationException: Can't convert to ComplexColor: type=0x1
    at android.graphics.drawable.VectorDrawable.applyTheme(636)
    at android.content.res.ResourcesImpl.loadDrawable(696)
    ... ToolbarWidgetWrapper.<init> ... PhoneWindow.installDecor ...
```

The vector has `android:tint="?0x01010429"` (`attr/colorControlNormal`, which
the theme resolves to `?attr/textColorSecondary`). `loadComplexColorForCookie`
rejects the value (`value.string == null` or the loaded XML tag is neither
`selector` nor `gradient`). Next: instrument `loadComplexColorForCookie` /
`nativeResolveAttrs`; `am_resolve_attrs` does not chase a resolved
`TYPE_REFERENCE` to its concrete color the way `fill_outputs` does.

## Files touched

* `crates/acos-framework-natives/src/res/theme.rs` (fix)
* `crates/acos-framework-natives/tests/xmlblock_diff.rs` (regression test)
* `crates/acos-vm/src/natives/gfx_vector.rs` (6 mechanical clippy fixes)
* `crates/acos-runtime/tests/m2.rs` (ignore reason)
* `results/m2/M2_LOG.md`, `results/m2/M2_STATUS_W107A.md`
* `results/m2/w107a-threadlocal-repro/{Main.java,classes.dex}` (repro evidence)

No commits made.
