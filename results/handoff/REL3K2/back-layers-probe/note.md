# back-layers probe (real ART, `acos serve --engine art`, REL3K2)

Command: `.cache/rel3k2/probe-back.sh` (kept in the log) — launch back-layers,
tap `dialog`, `key back`, then tap `input`, `key back`, `key back`.

| file | what it shows |
|---|---|
| `4-tree-dialog.json` | dialog window in the tree: `Confirm`, `Are you sure?`, `Cancel`, `OK` |
| `7-tree-after-back.json` | **dialog half OK**: after Back the dialog nodes are gone and the Activity's `Show dialog` / `Type here` / `Idle` are still there |
| `8-logs-after-back.json` | `dialog_shown` then `dialog_dismissed`; **no** `onBackPressed`, `onPause` or `onDestroy` — the Activity was never left |
| `13-logs-after-back-ime.json` | **IME half diverges**: the app reports `input_focus=true ime accepting=true active=true`, then Back #2 reaches the Activity's window (`dispatchKeyBack action=0/action=1`), `onBackPressed`, `onPause`, `onDestroy` — the task finished instead of the IME hiding |
| `11-back-ime.json` | the same Back's reply: `handled:true`, `settle.bound:"window-gone"`, `state_changed:true` |
| `12-tree-after-back-ime.json` | empty tree after the task finished |

Root cause of the IME half (code): the `InputBindResult` the ACOS IME manager
returns to the app has `channel: None` (`crates/acos-wm/src/ime.rs`,
`InputBindResult::success`; same in `crates/acos-services/src/ime.rs`). Without
an IME `InputChannel` the framework's `InputMethodManager.dispatchInputEvent`
takes the `mCurChannel == null` branch and returns `DISPATCH_NOT_HANDLED`, so
`ViewRootImpl.ImeInputStage` forwards Back to the view tree. AOSP's
`InputMethodService.onKeyDown`/`onKeyUp` (which hides the soft input and reports
the key handled) can never be reached.
