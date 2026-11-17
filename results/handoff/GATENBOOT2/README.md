# GATENBOOT2 — the Gaten bubble, end to end: app broadcasts reach their receivers, and the accessibility service is bound by the enable set

Branch `gatenboot2`, from `gatenboot` @ `9297db9c` (GATENBOOT's work) merged
with `modules` @ `7ad8dcb0` (MERGE030-era modules: PIFILLIN, BINDERREF2,
MPROC8, WV8). Worktree `.t3/worktrees/acos/t3-091c4459-ee524c3df993464c`,
2026-11-17. Private `ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET` under
`.cache/gatenboot2-run`, every ART run through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`, builds in
`.cache/gatenboot2-target` with `CARGO_PROFILE_*_DEBUG=0 … CARGO_INCREMENTAL=0`.

## Status

**DONE for both gaps, and the bubble scenario is green on real ART** —
`scenarios/gaten-bubble-overlay.yaml` passes 24/24 steps (the bubble's
`TYPE_ACCESSIBILITY_OVERLAY` windows on the stack, the bubble's visible content
in the tree, the switch that shows it, a tap on it that makes the app ask the
accessibility service for a screenshot, and the enable-set bind/unbind). One
caveat is measured and named in §5: the tap step's *harness-level* "the tree
changed" assertion can miss a hide/re-show transient (1 in 20 runs).

| Brief step | State |
|---|---|
| 1. App-originated broadcasts delivered (filter matching, package targeting, `REGISTERED_ONLY`, ordered/sticky honest) | **DONE** — §1. The receiver the Gaten bubble actually needs is a **manifest** `<receiver>`, not a dynamic one: it is now instantiated in the app's own process via `IApplicationThread.scheduleReceiver` |
| 2. The accessibility service binds on any **enable-set** change, and unbinds when its component leaves the set | **DONE** — §2 (`Settings.Secure` write or `service_ctl enable/disable`) |
| 3. `scenarios/gaten-bubble-overlay.yaml` flipped to `expectation: pass`: overlay window + content + tap | **DONE** (§3, 24/24 steps) with the tap caveat in §5 |
| 4. `gaten-bubble` and the module scenarios still pass on ART | **DONE** — §4 (10/10 scenarios) |
| 5. `cargo test -p acos-services -p acos-art -p acos-wm` | **DONE** — `acos-services` 73/73 targets green, `acos-wm` 10/10, `acos-art` all green except `tests/cleanup.rs`, which cannot run in a worktree this deep (§4.1) |

## 1. Broadcasts from the app (gap 1)

### What was actually blocking the bubble

The 2026-11-15 feedback case ("the bubble is a `TYPE_ACCESSIBILITY_OVERLAY`
window; ACOS cannot even start the service") reduced, after GATENBOOT, to one
wall: the switch's notification never reached the running service. Measured on
ART (`ACOS_ATMS_TRACE=1`), the app sends exactly what GATENBOOT predicted:

```
ACOSATMS broadcast method=broadcastIntentWithFeature reply=Int(1) receivers=3
  intent=(Some("com.gaten.app.action.FLOATING_BUTTON_CHANGED"), Some("com.gaten"), 0)
ACOSATMS broadcast-queued manifest=1 dynamic=0 dyn_records=[play-assetpacks,
  SCREEN_OFF/SCREEN_ON/USER_PRESENT, TRIGGER_LOOKUP]
```

So the intent **is** accepted and matched — by a **manifest** receiver
(`com.gaten.app.LookupTriggerReceiver`, declared in Gaten's manifest with
`com.gaten.app.action.FLOATING_BUTTON_CHANGED`), not by a dynamic one. The old
matcher turned that match into a *synthetic* `IIntentReceiver` id and the delivery
path dropped it ("manifest receiver … not delivered: an ACOS session has no
separate process to instantiate it"). That rationale is wrong for a receiver
whose package already has a bound app process: AOSP instantiates it **there**
(`BroadcastQueue` copies the intent, sets the receiver component, resolves its
`ActivityInfo` and calls `IApplicationThread.scheduleReceiver` →
`ActivityThread.handleReceiver`). The bubble's own `onReceive`
(`GatenAccessibilityService.f()` → `syncFloatingButton()`) is what adds the
overlay; disassembled from the APK's `classes.dex` to confirm the exact path:

```
Lcom/gaten/app/LookupTriggerReceiver;->onReceive:
  ... equals("com.gaten.app.action.FLOATING_BUTTON_CHANGED")
  sget-object GatenAccessibilityService.I; if null -> Log.w + return
  invoke-virtual GatenAccessibilityService;->f()V
```

### What changed

`crates/acos-services/src/ams.rs`

* `ReceiverFilter` — the `IntentFilter` facts a broadcast is matched on, and
  `ReceiverFilter::matches`, which is `IntentFilter.match` for a broadcast:
  * action: only a *named* action must be declared (an intent with no action
    matches any filter — the old code refused it);
  * categories: the intent's categories must be a subset of the filter's (the old
    rule added `CATEGORY_DEFAULT` and let a filter with no categories match an
    intent *with* categories — both wrong for receivers);
  * data/type: `IntentFilter.matchData` — a filter with no scheme and no type
    matches only an intent with neither; a scheme filter requires the scheme and
    then its scheme-specific part / authority+port / path (`DataPattern`
    implements `PatternMatcher`'s literal, prefix, **simple glob** and suffix
    kinds, and `DataAuthority` the `*.host`/port form); a type filter requires a
    matching MIME type with `*/*` and `x/*` wildcards, and never matches an
    intent that carries no type;
  * priority is recorded, never used: unordered delivery has no order.
* `AmsState::broadcast` — `BroadcastQueue`'s rules for the unordered case:
  * a package-targeted intent (`Intent.setPackage`) reaches only that package's
    receivers (a registration that names no package — the runtime-side mirror —
    cannot be excluded, and is documented as such);
  * `FLAG_RECEIVER_REGISTERED_ONLY` reaches no manifest receiver;
  * every matching receiver is queued, in the same process or not;
  * **ordered** (`resultTo != null`) is a structured `UNSUPPORTED_ANDROID_API`
    naming `sendOrderedBroadcast` (sequential delivery with
    `getResultCode`/`getResultData`/`abortBroadcast` is not modelled, and
    delivering the result immediately would be a fake). The old code delivered
    to `resultTo` with the *caller's* result code — a pass-shaped lie.
  * sticky: nothing distinguishes it on this wire, and `registerReceiver`
    already answers the honest "no sticky store" (a null `Intent`); the
    limitation is stated in the doc comment.
* `PendingManifestReceiver` + `AmsState::pending_manifest_receivers`: a manifest
  match is a *different* delivery, so it is queued where the owning process can
  be resolved instead of being dropped.

`crates/acos-services/src/atms.rs`

* `register_receiver` now hands the AMS the **whole** decoded `IntentFilter`
  (`filter_facts`), so scheme/type/authority/path matching is the filter's own
  (before, only actions+categories survived, so a data-only filter matched
  everything and a type filter never matched anything).
* `take_manifest_receiver_matches` drains the manifest queue and reports (module
  log, `Warn`) a match whose package has no running process — a device would
  start the process; that is the launcher's decision, never this host's.
* `AtmsState::attached_thread_for_package` resolves that process the way
  `ActivityManagerService.getProcessRecordLocked` does, in precedence order: a
  `ProcessRecord` that names the package → the process running one of the
  package's activities (the launch record names the package, its token names the
  process tag) → a session with exactly one attached process whose own record
  names no package (the ART launcher's handshake does not always carry the
  package name).

`crates/acos-services/src/host.rs`

* `take_manifest_receiver_deliveries` builds the AOSP transaction:
  `Intent` copy with the receiver component, `ActivityInfo` from the **installed
  package** (the same `ApplicationInfo` the process was bound with, so
  `ActivityThread.getPackageInfoNoCheck` reuses the bound `LoadedApk`: class
  loader, data dir and all), `CompatibilityInfo.DEFAULT`, `resultCode = 0`,
  null data/extras, `sync = false`, `assumeDelivered = false`,
  `sendingUser = 0`, `processState = PROCESS_STATE_RECEIVER (11)`,
  `sendingUid = 0`, `sendingPackage` = the intent's package. It is addressed at
  the owning process's `IApplicationThread` and travels on the same delivery
  queue every other `system -> app` call uses.
* It runs in `take_deliveries` (after every transaction and control op) and in
  `take_timed_deliveries` (the peer's idle tick), so a host-originated broadcast
  (`inject_broadcast`, a fired `PendingIntent`) reaches a manifest receiver too.

### The one launch-flag fix that was needed to run Gaten at all

MERGE030-era `AmsState::start_activity` ORs the intent's flags into the launch
flags (correct) but refuses everything outside `NEW_TASK|CLEAR_TOP|SINGLE_TOP`.
Gaten's `LauncherActivity` starts `MainActivity` with
`NEW_TASK|RESET_TASK_IF_NEEDED` (`0x10200000`), so on the merged tree the app
died at its own launcher screen with
`UnsupportedOperationException: … startActivity flags 0x10200000` before any of
this work could run. `FLAG_ACTIVITY_RESET_TASK_IF_NEEDED` is accepted now: AOSP
uses it only when an existing task is reused for a new root, and ACOS's launch is
always that task's root, so there is nothing to reset (pinned by
`ams::tests::reset_task_if_needed_is_accepted_and_launches`; the refusal of a
genuinely unsupported flag stays pinned).

## 2. The accessibility service and the enable set (gap 2)

AOSP's `AccessibilityManagerService` learns that the enable set changed from its
`SettingsObserver` → `onUserStateChangedLocked` → `readConfigurationForUserStateLocked`
→ `setEnabledServices` → `bindServices` (and `removeServiceLocked` →
`unbindService` for a service that left the set). ACOS bound a service only from
the app's `IAccessibilityManager.addClient`, so switching the service on after
the app had started never bound it, and switching it off never unbound it.

`crates/acos-services/src/a11y_bind.rs`

* `sync_enabled_services(state)` is the observer, as a **pull** at the session's
  delivery drain: it re-reads `Settings.Secure.ACCESSIBILITY_ENABLED` and
  `ENABLED_ACCESSIBILITY_SERVICES`, and when the namespace **generation** or the
  pair changed it updates the module state and reconciles the bind — unbind the
  service that left the set, bind the one that joined it. A table nobody has
  written (no keys at all) is not an authority, so it cannot fight a state a
  harness configured directly.
* `plan_unbind` is the inverse of `plan_bind`:
  `scheduleUnbindService(token, intent)` + `scheduleStopService(token)` at the
  app thread (the pair `crate::service_bind` already uses), the overlay window
  token released (AOSP removes an accessibility window token with its service)
  and the `IAccessibilityServiceClient` reference dropped.
* `A11yState.observed_secure_generation` is the observer's trigger.

`crates/acos-services/src/misc.rs`

* `A11yState::enable_services`/`disable` now write the two keys through
  `SettingsState::put`, i.e. they bump the namespace generation the way a real
  settings write does (`Settings$GenerationTracker`). Without that, the module's
  own write was invisible to the observer.
* `A11yBoundService.token` records the bind token the unbind pair names.
* `A11yService::sync_from_settings` is the object-facing entry point.

`crates/acos-services/src/host.rs`: `take_accessibility_deliveries` (in both
delivery drains) is where the reconciliation's deliveries are produced.

`crates/acos-services/src/modules/accessibility.rs`: two control ops, the two
directions of the Settings UI switch — `enable {services: [...]}` and `disable`.
Both only write the settings, so the module cannot reach a state the settings
table does not describe; the bind/unbind follows from the write.

Pinned by `crates/acos-services/tests/gatenboot2_a11y_settings.rs` (4 tests):
a settings write binds (`scheduleCreateService` + `scheduleBindService`), removing
the component unbinds (`scheduleUnbindService` + `scheduleStopService`), an
unchanged set is a no-op (the drain runs after every transaction, so a rebind
loop would be fatal), replacing the component unbinds the old and binds the new
in one reconciliation, and the module's own `disable`/`enable` ops go through the
very same path.

## 3. The bubble scenario, on real ART

`scenarios/gaten-bubble-overlay.yaml` — **pass, 24/24 steps**
(`regression/gaten-bubble-overlay/`). Its assertions, in order:

1. the module starts with nothing enabled and nothing bound
   (`enabled: false, service_bound: false`);
2. `service_ctl accessibility enable` (a `Settings.Secure` write) binds the
   service — the app's `addClient` is *not* involved (gap 2, measured on a device
   path: `service_bound: true` afterwards);
3. onboarding + Settings + Screen lookup, then the "Floating button" switch:
   `Assert` the app's broadcast reaches `LookupTriggerReceiver` in its own
   process, whose `onReceive` adds the bubble (gap 1);
4. `assert_windows` — the stack is
   `[application, accessibility_overlay, accessibility_overlay]`
   (`TYPE_BASE_APPLICATION`, `TYPE_ACCESSIBILITY_OVERLAY` ×2): the bubble and the
   "remove target" window Gaten adds (hidden) for dragging;
5. `assert_visible` `content_desc == "Screen lookup"` — the bubble's own content,
   in the tree (the probe's `windows[]` block carries both overlay windows with
   real WMS frames: the bubble at `[914,1080][1059,1225]`, the remove target at
   `[467,2171][612,2316]`);
6. `assert_not_visible` `content_desc == "Remove floating button"` — the remove
   target is on the stack but not visible until a drag;
7. **the tap**: after a short host-side wait (the overlay layer takes input a
   moment after it is added) a tap on the bubble's content; the app starts its lookup and asks
   the accessibility module for a screenshot, which the module serves from the
   composited frame — `service_ctl accessibility status` shows
   `screenshots: [{display_id: 0, status: 0}]` (`TAKE_SCREENSHOT_SUCCESS`);
8. `service_ctl accessibility disable` unbinds the service
   (`service_bound: false`), which is the other half of gap 2.

`settle: content` on the switch is deliberate: while the bubble is alive the
screen keeps changing (Gaten animates it), so a strict pixel/event settle can
never complete; the harness's content-stable rule backs UI/text/window
assertions. There is no post-bubble `capture: {fresh: true}` for the same reason
(the run's own `frames/` are the pictures).

## 4. Regression (ART, shared lock, private runtime dir) and the unit suites

All green (`regression/<name>/verdict.json`):

```
gaten-bubble          pass 14/14   a11y-probe         pass 15/15
gaten-bubble-overlay  pass 24/24   notif-perm-granted pass 10/10
notif-perm-denied     pass  5/5    location-probe     pass 12/12
location-live-attach  pass 18/18   location-live-detach pass 12/12
sensors-scripted      pass 16/16   sensors-off        pass  5/5
```

`cargo test -p acos-services` — **73 test targets, 0 failed**
(`tests-services.log`), including the new `gatenboot2_a11y_settings`, the
rewritten `ams` matcher tests (`the_action_and_category_rules_are_aosp_intent_filter_match`,
`the_data_and_type_rules_follow_intent_filter_match_data`,
`authorities_ports_and_paths_match_like_intent_filter`,
`package_targeting_and_registered_only_follow_the_broadcast_queue`,
`an_ordered_broadcast_is_a_structured_unsupported`), `moda11y6_windows_changed`,
`mproc8_service_bind`, `process_initializers`.

`cargo test -p acos-wm -p acos-art` — `tests-art-wm.log`: `acos-wm` 77 lib + 10
targets green, `acos-art` 169 lib tests + 13 integration targets green.

### 4.1 The one environment caveat

`crates/acos-art/tests/cleanup.rs` (6 of 8 tests) cannot run in this worktree:
the tests build their scratch dir under the repository root and bind unix
sockets there, and
`/home/Qui/.t3/worktrees/acos/t3-091c4459-ee524c3df993464c/.cache/h1-tests/<tag>-<pid>/…`
exceeds `SUN_LEN` (`acos-art: io: path must be shorter than SUN_LEN`). They never
reach `acos-art`'s code path this branch touches; `live_view` has the same
worktree-depth sensitivity (GATENBOOT recorded it). Not caused by this change and
not fixable from here.

Two more merge-time test adjustments, both semantics the merge made stale:

* `process_initializers::two_attached_modules_forward_two_initializers_in_attach_order`
  was removed: GATENBOOT's invariant (AOSP's per-process
  `initializeMainlineModules` installs the telephony/bluetooth managers, so the
  modules declare `&[]`) makes "two modules, two initializers" unreachable. The
  file's own comment records why.
* `moda11y6_windows_changed::bind_service` now writes the two `Settings.Secure`
  keys as well: a bound service *is* an enabled one, and since gap 2 the settings
  table is the authority the bind is reconciled against.

## 5. What is NOT done, measured

* **The tap step is timing-sensitive** (1 failure in 20 consecutive runs, every
  other step deterministic). Gaten's lookup cannot complete in this image (no ML
  Kit recognition pack), so the bubble hides for the lookup and re-shows a moment
  later; the harness requires *every* tap to change the UI tree, and that
  hide/re-show can fall between two probe samples. The lookup's *request* (the
  screenshot record) is what the scenario asserts, and it is present in the
  failing runs too. The smallest next action is a harness flag
  (`tap … expect_change: false`, the SE1 spelling already used for pixels) or a
  scan for the app's own transient; changing the scenario cannot fix a sampling
  race.
* **The lookup sheet / OCR** needs the recognition pack, which is not in the
  AOSP image; the tap reaches the service and the screenshot is delivered
  (`status: 0`), the sheet is the app's next step.
* **A strict observation bound to the tap kills the guest.** With
  `tap: {…, settle: content}` (a strict settle after the tap) the guest dies with
  `SIGSEGV` in `android::GraphicBufferMapper::getAllocationSize` (`libui.so`,
  `tap-wall/native-crash.txt`): once the app wraps the screenshot
  `HardwareBuffer` in a `Bitmap` on a worker thread, a probe request that lands in
  that window faults in libui's own `getAllocationSize` (the gralloc shim
  interposes the mapper's entry points, but this call resolves libui's internal
  copy, whose HAL mapper object is null in a gralloc-less guest). `settle: fast`
  keeps the assertion off that window. This is the accessibility/screenshot
  track's wall (A11YNATIVE), not a broadcast or bind gap; the crash stack is kept
  as evidence so it can be fixed where it belongs.
* Dragging needs paced `dispatchGesture` (A11YPACE) and was not attempted, as the
  brief said.
* Sticky broadcasts and ordered broadcasts stay structured refusals (§1).

## 6. Repro

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR=$PWD/.cache/gatenboot2-run
export ACOS_SESSION_SOCKET=$PWD/.cache/gatenboot2-run/session.sock
export ACOS_VIEW_TEST_DIR=$PWD/.cache/lv
export ACOS_ART_BIN=$PWD/.cache/gatenboot2-target/release/acos-art
tools/art-runtime/run-exclusive.sh --task GATENBOOT2 -- tools/guarded.sh --mem 10G --time 700 -- \
  env -u ACOS_HOME .cache/gatenboot2-target/release/acos verify --engine art \
  --scenario scenarios/gaten-bubble-overlay.yaml --json \
  --artifacts results/handoff/GATENBOOT2/regression/gaten-bubble-overlay
```

`ACOS_ATMS_TRACE=1` prints the broadcast/receiver decisions
(`ACOSATMS broadcast …`, `receiver-registered …`, `manifest-matches …`,
`scheduleReceiver …`); `.cache/run-suites.sh` (in the worktree) is the batch that
produced §4.

## 7. Files

* `crates/acos-services/src/ams.rs` — `ReceiverFilter`/`DataPattern`/`DataAuthority`
  + AOSP matching, `broadcast` (package targeting, `REGISTERED_ONLY`, ordered
  refusal), `PendingManifestReceiver`, `FLAG_RECEIVER_REGISTERED_ONLY`,
  `RESET_TASK_IF_NEEDED` accepted, tests.
* `crates/acos-services/src/atms.rs` — full filter facts into the AMS,
  `take_manifest_receiver_matches`, `attached_thread_for_package`, traces.
* `crates/acos-services/src/host.rs` — `take_manifest_receiver_deliveries`
  (`scheduleReceiver`), `take_accessibility_deliveries` (both drains),
  `manifest_activity_info`.
* `crates/acos-services/src/a11y_bind.rs` — `sync_enabled_services`,
  `plan_unbind`, `SCHEDULE_UNBIND_SERVICE`/`SCHEDULE_STOP_SERVICE`.
* `crates/acos-services/src/misc.rs` — `enable_services`/`disable` write through
  `put`, `A11yBoundService.token`, `observed_secure_generation`,
  `sync_from_settings`.
* `crates/acos-services/src/modules/accessibility.rs` — `enable`/`disable`
  control ops.
* `crates/acos-services/tests/gatenboot2_a11y_settings.rs` (new),
  `crates/acos-services/tests/moda11y6_windows_changed.rs` (helper),
  `crates/acos-services/tests/process_initializers.rs` (stale test removed).
* `scenarios/gaten-bubble-overlay.yaml` (flipped to `pass`), evidence under
  `results/handoff/GATENBOOT2/`.

## 8. Environment

Target `.cache/gatenboot2-target` (dev/test/release `debug=0`, incremental off);
peak RSS 253 MB and 10.9 s wall for the Gaten scenarios; disk stayed at 72–74 %
of `/`. Fixture APKs (`a11y-probe`, `notif-perm-probe`, `location-probe`,
`sensor-probe`, `proc-a`, `proc-b`) were copied into `fixtures/build` from a
sibling worktree whose fixture sources are byte-identical (the main checkout's
`fixtures/build` had no APKs); the copy and the target were deleted at the end.
