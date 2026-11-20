# GATENBACK3 — Back Closes Gaten's Lookup Sheet (`TYPE_ACCESSIBILITY_OVERLAY`)

## Summary

With Gaten's Compose lookup sheet open (`TYPE_ACCESSIBILITY_OVERLAY`), pressing `key: back` was previously acknowledged as `handled=true`, yet the sheet remained open while tapping `Close lookup` dismissed it.

**Root cause (Link A — `ViewRootImpl.ImeInputStage` / `InputMethodManagerService`):**
`MainActivity`'s dictionary search `EditText` (`"猫, ねこ, neko..."`) showed the soft input (`ImeState.ime_visible = true`). When Gaten's non-editor `TYPE_ACCESSIBILITY_OVERLAY` lookup window gained focus and called `IInputMethodManager.startInputOrWindowGainedFocus`, ACOS's `ImeWireService::start_input` discarded `windowToken`, `startInputFlags`, `softInputMode`, and `windowFlags` and never ran AOSP Android 14's `ImeVisibilityStateComputer.computeState` focus-change hide rules. As a result, `ime_visible` stayed `true` across the window focus change. When `key: back` arrived on the focused overlay window, `ViewRootImpl.ImeInputStage` sent it down the IME `InputChannel`, where ACOS's `ImeState::service_pending_keys()` saw `ime_visible == true` and swallowed both `ACTION_DOWN` and `ACTION_UP` as `handled=true` (`ACOSIME back-down hid the soft input (seq=1)`). `OverlayRoot.dispatchKeyEvent` never saw the key.

## Before vs. After Trace

### Before (GATENBACK2 `p1/launcher.stderr.log` on `7dd03e50d`)

```text
7920: ACOSW13 ime startInput sequence=5 client=Some(4294967307) recorded=0 editor={inputType=16385 imeOptions=3 package=Some("com.gaten") fieldId=944 hint=Some("猫, ねこ, neko...") ...} -> SUCCESS
7921: ACOSIME showSoftInput client=0x10000000b shown=true
9926: ACOSW13 ime startInput sequence=1 client=Some(4294967307) recorded=0 editor={inputType=0 imeOptions=0 package=Some("com.gaten") fieldId=0 hint=None ...} -> SUCCESS
10623: ACOSIME back-down hid the soft input (seq=1)
```

- At line 9926 the lookup overlay gained focus (`inputType=0`), but `ime_visible` stayed `true`.
- At line 10623 `key: back` was consumed by the stale soft-input state instead of reaching `OverlayRoot.dispatchKeyEvent`.

### After (GATENBACK3 `wl-1/launcher.stderr.log`)

```text
ACOSW13 ime startInput sequence=5 client=Some(4294967307) window=Some(4294967310) flags=0x7 softInputMode=0x10 windowFlags=0x80810100 recorded=0 editor={inputType=16385 imeOptions=3 package=Some("com.gaten") fieldId=944 hint=Some("猫, ねこ, neko...") ...} -> SUCCESS
ACOSIME showSoftInput client=0x10000000b shown=true
ACOSIME startInput hid the soft input (reason=HIDE_UNSPECIFIED_WINDOW window=0x100000030 flags=0xd softInputMode=0x20 windowFlags=0x10003a0)
ACOSW13 ime startInput sequence=1 client=Some(4294967307) window=Some(4294967344) flags=0xd softInputMode=0x20 windowFlags=0x10003a0 recorded=0 editor={inputType=0 imeOptions=0 package=Some("com.gaten") fieldId=0 hint=None ...} -> SUCCESS
```

- When the lookup overlay (`0x100000030`) gains focus (`flags=0xd`, `softInputMode=0x20`, `windowFlags=0x10003a0`, `mayUseInputMethod=true`, `!IS_TEXT_EDITOR`), ACOS hides the stale soft input (`HIDE_UNSPECIFIED_WINDOW`), matching AOSP `ImeVisibilityStateComputer.computeState` (`frameworks/base/services/core/java/com/android/server/inputmethod/ImeVisibilityStateComputer.java`).
- `key: back` then passes through `ImeInputStage` unhandled and reaches `OverlayRoot.dispatchKeyEvent` -> Compose `BackHandler { close() }` -> exit transition -> `onDismiss()`.

## Physical Phone Confirmation (`adb -s ZA222WGJPP`, Moto G35 5G)

Confirmed on real Android hardware with Gaten installed (`com.gaten`) and `GatenAccessibilityService` enabled:
1. Dictionary search `"neko"` open with soft keyboard up: `dumpsys input_method` reports `mInputShown=true`.
2. Lookup overlay triggered (`com.gaten.app.action.TRIGGER_LOOKUP`, `type=2032` `TYPE_ACCESSIBILITY_OVERLAY`): Android IMMS immediately flips `mInputShown=false` on window focus gain.
3. Word sheet opened (`mInputShown=false`), single `input keyevent 4` (Back): immediately removes the `type=2032` overlay window and returns to `MainActivity`.

## Fix

Implemented in one place (`crates/acos-services`):
- `crates/acos-services/src/ime.rs`: Added AOSP API 34 `StartInputFlags`, `SOFT_INPUT_*` masks/states, `may_use_input_method(window_flags)`, `StartInputContext`, per-window focus tracking (`focused_window`), explicit/forced show flags (`requested_show_explicitly`, `show_forced`), `start_input_with_context(...)` implementing `ImeVisibilityStateComputer.computeState` focus-change hide/show rules (`HIDE_UNSPECIFIED_WINDOW`, `HIDE_STATE_HIDDEN_FORWARD_NAV`, `HIDE_ALWAYS_HIDDEN_STATE`, `HIDE_NOT_ALWAYS`, `CLEAR_SHOW_FORCED_FLAG_WHEN_LEAVING`), and `START_INPUT_REASON_WINDOW_FOCUS_GAIN_REPORT_ONLY` handling without tearing down a live `InputChannel`.
- `crates/acos-services/src/ime_wire.rs`: Decoded `windowToken`, `startInputFlags`, `softInputMode`, `windowFlags`, and `targetSdkVersion` from `IInputMethodManager.startInputOrWindowGainedFocus` and forwarded them to `ImeState::start_input_with_context`, returning `InputBindResult.SUCCESS_REPORT_WINDOW_FOCUS_ONLY` when appropriate.
- `crates/acos-services/src/wms.rs`: Updated `WmsState::set_ime_visible(false)` to clear `inset_type::IME` on every window currently holding it (such as the previously focused `MainActivity` editor window) rather than only the newly focused non-editor overlay window.
- `crates/acos-services/tests/ime_wire.rs`: Added 4 regression tests covering non-editor overlay focus hiding stale IME so Back reaches the overlay, `SOFT_INPUT_STATE_*` / `FLAG_ALT_FOCUSABLE_IM` / same-window editor rules, `WINDOW_FOCUS_GAIN_REPORT_ONLY`, and WMS IME inset clearing on the previous window.
- `scenarios/gaten-word-lookup.yaml`: Replaced the `Close lookup` tap workaround with `key: {key: back, settle: fast, expect_change: false}` and asserted the sheet is gone (`Close lookup` and `Meanings` not visible) and the floating bubble (`Screen lookup`) and 3-window stack (`[application, accessibility_overlay, accessibility_overlay]`) are restored.

## Verification Results

### 1. Extended `scenarios/gaten-word-lookup.yaml` (5/5 PASS)

| Rep | Verdict | Wall Time | Steps | Step 40 (`key: back`) |
| --- | --- | --- | --- | --- |
| 1 | `pass` | 25,852 ms | 45/45 | `handled=true`, `ack_ms=10`, `first_change_ms=76`, `state_changed=true` |
| 2 | `pass` | 25,845 ms | 45/45 | `handled=true`, `ack_ms=10`, `first_change_ms=65`, `state_changed=true` |
| 3 | `pass` | 25,901 ms | 45/45 | `handled=true`, `ack_ms=10`, `first_change_ms=86`, `state_changed=true` |
| 4 | `pass` | 25,872 ms | 45/45 | `handled=true`, `ack_ms=10`, `first_change_ms=77`, `state_changed=true` |
| 5 | `pass` | 26,228 ms | 45/45 | `handled=true`, `ack_ms=10`, `first_change_ms=67`, `state_changed=true` |

Compact visual evidence from rep 1:
- Before Back (word sheet open): `wl1-before-back-word-sheet.png` (frame 249)
- After Back (sheet dismissed, bubble restored): `wl1-after-back-bubble.png` (frame 290)

### 2. Regression Gates (ALL PASS)

| Gate | Verdict | Wall Time | Notes |
| --- | --- | --- | --- |
| `scenarios/gaten-bubble-overlay.yaml` | `pass` | 16,208 ms | 28/28 steps |
| `scenarios/back-layers.yaml` | `pass` | 4,355 ms | 22/22 steps (dialog -> IME -> Activity Back order intact) |
| `scenarios/back-layers-ime.yaml` | `pass` | 4,591 ms | 22/22 steps |
| `scenarios/gaten-backstack.yaml` | `pass` | 11,386 ms | 25/25 steps |
| `cargo test --release --offline -p acos-services` | `ok` | — | 741 passed, 0 failed, 2 ignored (includes 17 `ime_wire` tests, 4 new) |

## GBMERGE verification (2026-11-20)

Verified `gbmerge` (GATENBACK3 + `scenarios/gaten-word-lookup.yaml` Close-lookup extension + latest `origin/modules` merge) with `acos verify --engine art` (release `acos` + `acos-art`) and full `acos-services` tests:

### 1. Scenario runs (7/7 PASS)

| Scenario | Verdict | Steps | Wall ms | Evidence |
| --- | --- | --- | --- | --- |
| `scenarios/back-layers.yaml` | `pass` | 22/22 | 4,353 ms | `evidence/gbmerge/back-layers.json`, `evidence/gbmerge/back-layers-verdict.json` |
| `scenarios/back-layers-ime.yaml` | `pass` | 22/22 | 4,640 ms | `evidence/gbmerge/back-layers-ime.json`, `evidence/gbmerge/back-layers-ime-verdict.json` |
| `scenarios/gaten-backstack.yaml` | `pass` | 25/25 | 11,443 ms | `evidence/gbmerge/gaten-backstack.json`, `evidence/gbmerge/gaten-backstack-verdict.json` |
| `scenarios/gaten-bubble-overlay.yaml` | `pass` | 28/28 | 16,324 ms | `evidence/gbmerge/gaten-bubble-overlay.json`, `evidence/gbmerge/gaten-bubble-overlay-verdict.json` |
| `scenarios/gaten-word-lookup.yaml` (rep 1) | `pass` | 64/64 | 34,745 ms | `evidence/gbmerge/gaten-word-lookup-1.json`, `evidence/gbmerge/gaten-word-lookup-1-verdict.json` |
| `scenarios/gaten-word-lookup.yaml` (rep 2) | `pass` | 64/64 | 33,164 ms | `evidence/gbmerge/gaten-word-lookup-2.json`, `evidence/gbmerge/gaten-word-lookup-2-verdict.json` |
| `scenarios/gaten-word-lookup.yaml` (rep 3) | `pass` | 64/64 | 33,980 ms | `evidence/gbmerge/gaten-word-lookup-3.json`, `evidence/gbmerge/gaten-word-lookup-3-verdict.json` |

### 2. Cargo tests

- `cargo test -p acos-services` (all targets, 90 test binaries/doctests): **741 passed, 0 failed, 2 ignored** (includes 17/17 `tests/ime_wire.rs` tests).
