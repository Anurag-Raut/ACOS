# MERGE040B — the five v0.4.0 module tracks merged into `modules`

Branch `merge040b` from `modules` @ `473859c2` (merge040a + MERGEFIX2 + sysui4).
One commit per merge, in the brief's order; each conflict resolved to **one**
implementation per seam. Then one behaviour fix and one merge-artifact fix,
both found by the acceptance corpus. This file is the conflict table + intent,
then the verification, then the two things the merge surfaced.

```
473859c2 modules (merge040a + MERGEFIX2 + sysui4)     <- base
404b799c MERGE040B(1/5) wvtap         0faabef0
94b9bb8c MERGE040B(2/5) scenhyg       8b3afbdd
aed14fa1 MERGE040B(3/5) launchextras  dd16c852
12b2689a MERGE040B(4/5) binderref4    162d86a0       (contains binderref3 a0024b71)
71f27e3c MERGE040B(5/5) procuid       bfd5292c       (contains binderpid)
e7976c02 MERGE040B: the primary record's uid is unknown at attach (test)
c7236c42 MERGE040B: a declared dangerous permission is not an install-time grant
659e224d MERGE040B: one webview tap implementation (WVFAIR's), not a hybrid  <- HEAD
```

`cargo check --workspace --all-targets`: green after every merge
(`01-…05-cargo-check-after-*.log`).

## 1. Conflicts and resolutions

### 1.1 merge 1 (wvtap) — no textual conflict, seams re-checked

Auto-merged. WVTAP's `InputHost` now owns the routing rule and needs the layer
stack (`with_window_stack`), so `launch.rs`'s duplicate `hit_test` tap arm is
gone and `tap`/`swipe` go through `live.input.tap/swipe`; `tap_window` stays as
the explicit-token primitive `tap()` uses. Checked for the MERGEFIX lesson
(duplicate constructors / shadowed `let`s): one `ProbeState::for_session`, one
`InputHost::from_env`, no duplicated arm.

### 1.2 merge 2 (scenhyg) — two conflicts

**`tools/build-apps.sh`.** HEAD carried MERGE040A's single union loop; SCENHYG
carried its own (shorter) loop plus the finding that the duplicate `for … do`
header pair also left the loop unterminated, so *nothing* below it built.

Kept: **HEAD's loop** (already the union — it names `sensor-probe`,
`notif-perm-probe` and sysui3a's `permissioncontroller perm-probe systemui`),
with one comment carrying both reasons. `bash -n` clean.

**`crates/acos-webview/src/view.rs`** (the tap/touch input path). Both sides
fixed the same symptom — a tap's down→up gap stretching into a long press when
another view pulled a frame:

* **modules/WVFAIR:** keep `down → frame → up → frame`, but the two pulls are
  `render_input` (fair gate, `Input` priority, granted ahead of guest pulls).
* **scenhyg:** remove the mid-gesture pull, pull once after the gesture, and
  wait only 150 ms on a non-final phase's CDP reply.

Kept: **WVFAIR's** (input-priority pulls between the phases; that is the
mechanism the merged tree measures 3/3 with, and the 150 ms wait is subsumed
because both acks are already waited for *after* both events). My first
resolution kept both halves (WVFAIR's two pulls **and** SCENHYG's trailing
render — three pulls per tap) and webview-probe rep 2 of 3 failed at
`assert_text text=="tapped"`. Fixed to one implementation (`659e224d`); the
file is now byte-identical to `modules`' and webview-probe/fair are 3/3 + 3/3.
SCENHYG's rationale is not lost: §2.1 of this file and the scenario comment in
`scenarios/webview-probe.yaml` keep the 4/10 → 2/10 → 3/3 history.

### 1.3 merge 3 (launchextras) — one conflict

**`tools/build-apps.sh`**: HEAD's union (`… permissioncontroller perm-probe
systemui`) vs launchextras' `launch-extras-probe`. Kept: the **union**
(`notif-perm-probe permissioncontroller perm-probe systemui launch-extras-probe`)
in the one loop.

`scenarios/launch-extras-unsupported.yaml` (SCENHYG's structured refusal of
`launch.extras`) is deleted by launchextras' own author: the fields are carried
now. `scenarios/launch-extras-parcelable.yaml` is the remaining structured
refusal (a `Parcelable` extra needs the app's class loader).

Launch seams checked: one `ProbeState::for_session(top)`, one `LaunchOpts.intent`
(`protocol::LaunchIntent`, `ExtraValue` typed), the guest unmarshals the host's
`Intent.writeToParcel` bytes through the framework's own `Intent.CREATOR` and
only the "caller declared nothing" path still builds the stock `ACTION_MAIN`
Intent. MERGEFIX2's `PendingLaunch.origin` / `pending_launch_for_tag` and
SYSUI3A's `set_primary_package` are untouched by this merge.

### 1.4 merge 4 (binderref4) — no textual conflict

GuestRef callbacks + binder trace sit beside the merged code; both sides'
`refs.rs` additions auto-merged. Checked: one ref ledger, no second
`GuestRef`-to-wire mapping.

### 1.5 merge 5 (procuid) — six conflicts

**`tools/build-apps.sh`** (again): the same one loop, now the union of *every*
side. All three branches' notes about the MERGE030 duplicate header are folded
into one comment (they are three observations of one defect: the shadowing loss
of `sensor-probe`/`notif-perm-probe`, the unterminated loop, and the
`permissioncontroller`/`perm-probe`/`systemui`/`launch-extras-probe` fixtures).

**`crates/acos-binder-peer/src/peer.rs` `tick()`**: SYSUI1 drains host
transactions on the idle turn (a host-initiated launch must reach a running
process); BINDERPID stamps every guest-bound frame's sender. Kept **both**, with
the stamping **once, after the last source** — the same place and order as
`handle_tagged`, so a host transaction is stamped like any other delivery.

**`crates/acos-services/src/atms.rs` `process_started()`**: procuid's 4-arg
signature (`pid` from the kernel's credential) plus SYSUI2's rule that the
package name is filled only when the record has none (`0`/empty = "unknown
here", never a value to keep over a real one). One doc block carries both.

**`crates/acos-services/src/permissions.rs`** and
**`src/modules/permissions.rs`**: purely additive on both sides — SYSUI1's
dialog path (`dialogs`, `record_request`, `apply_dialog_result`, the
`getPermissionInfo`/`getPermissionGroupInfo`/`queryPermissionsByGroup`/
`getAllPermissionGroups` arms) and PROCUID's per-uid path (`SYSTEM_UID`,
`uid_grants`, `set_uid_grants`, `check_uid_permission`, the
`checkUidPermission` arm). Kept **both**; both sides' fields initialised in
`for_package`/`Default`.

**`crates/acos-services/src/pms.rs` `set_package_from_apk_as()`**: both sides
re-point the session's one permission table at the newly installed APK. Kept
**one** re-point that sets install-time grants, the package's own
`<permission>` declarations *and* the `uid_grants` snapshot, from the one new
`PmsState`, under the one lock — the two comments merged into one that says
why (a replacement that skips either leaves the table describing the previous
APK).

**`crates/acos-services/tests/sysui4_result_ordering.rs`**: call sites take the
pid (`0` = unknown).

Protocol: `PROTOCOL_VERSION = 4` (procuid/binderpid wins), and the byte tests
that pin the sender identity at header bytes 40..48 and a v3 header decoding as
an unknown sender are intact in `acos-binder-peer/src/protocol.rs`.

## 2. What the merge surfaced (and how it was fixed at the root)

### 2.1 `sysui1-permission-allow`: a declared dangerous permission answered GRANTED

`c7236c42`. After the merge the scenario failed at its first assertion —
`state before: CAMERA=0 LOCATION=0` instead of `-1`.

Cause, traced on both trees (`MERGE040B_TRACE_UID`, launcher stderr): the
guest's `checkSelfPermission` reaches `IActivityManager.checkPermission` →
`AtmsState::check_permission_for_uid` → `PermissionStore::check_uid_permission`
→ (package row matches) → `PermissionStore::check()`. `check()` answered from
**`install_time`**, which is the manifest's `<uses-permission>` list — a
*request*, not a grant — so `is_declared("android.permission.CAMERA")` granted
it. On `473859c2` the same call had a **denied entry** for both permissions in
the table (so the entry branch answered `-1`) and the fallback was never
reached; with procuid's uid path the first check arrives with an empty entry
table and the fallback decided the answer. (Reproduced: the `modules` tree at
`473859c2`, built in a scratch worktree, passes; the merged tree fails.)

Fix: the one place the answer is decided now knows AOSP's rule —
`permission_catalog::PERMISSIONS` is exactly the platform's dangerous set
(`protectionLevel` 0x1, what the image's core manifest declares), and a
dangerous permission is never granted from declaration alone. The entry (the
dialog's grant, a pre-grant config) still wins; normal permissions keep their
install-time grant. Unit test
`a_declared_dangerous_permission_is_not_an_install_time_grant`.

### 2.2 `webview-probe`: one implementation per seam, not two

`659e224d`, see §1.2. The hybrid (WVFAIR's pulls + SCENHYG's trailing render)
failed 1/3; the one implementation passes 3/3 + fair 3/3.

### 2.3 The all-caps `INCREMENT` label GLSPIKE2 reported — not present here

`18-label-case.txt`. Both arms pass, and the counter's own ART tree reports the
button as `Increment` (7 samples), no `INCREMENT` node in either arm. No
bisect needed: this tree does not show the regression GLSPIKE2 saw on freshly
built binaries of its branch.

## 3. Verification

Environment: private `ACOS_RUNTIME_DIR=$WT/.cache/rt`, `ACOS_SESSION_SOCKET=…/
rt/session.sock`, `ACOS_VIEW_TEST_DIR=$WT/.cache/lv`, `ACOS_FONT_DIR` from the
shared boot image. Fixtures: `tools/build-apps.sh` (17), `tools/build-dogfood-
apps.sh`, `tools/build-fixtures.sh` (dexprogs 28), `fixtures/build-hostile-
probe.sh` (the negative matrix needs `hostile-forest`). Every ART run went
through `tools/art-runtime/run-exclusive.sh --task MERGE040B`.

| gate | result | evidence |
|---|---|---|
| `cargo check --workspace --all-targets` (after each merge) | green ×5 | `0[1-5]-cargo-check-after-*.log` |
| `cargo test -p acos-protocol -p acos-wm -p acos-binder-peer -p acos-webview -p acos-netd` | pass | `10-test-nonart.log` |
| `cargo test -p acos-services -p acos-verify -p acos-wm` | pass (95 binaries) | `11-test-services-verify-wm.log` |
| `cargo test -p acos-runtime -p acos-mcp` | pass (54) | `12-test-runtime-mcp.log` |
| `cargo test -p acos-cli` | pass (26) | `13-test-cli.log` |
| `cargo test -p acos-art` | pass (16) | `14-test-art.log` |
| the four above again after the two fixes | pass (128) | `15-test-post-fix.log` |
| ART corpus, 35 runs | 34 pass, 1 expected `unsupported` | `art/*/art/verdict.json` |
| `tools/run-matrix.sh --no-build` | false_pass 0, infra_timeout 0, unclassified 0, negative_correct 32/32, step_timeout 3 | `16-matrix-no-build.log`, `matrix.json` |
| `tools/check-tree-clean.sh` | clean | `check-tree-clean: clean (659e224d, 0 tracked change(s))` |

The matrix ran with `--allow-contaminated` (another agent's release build had
loadavg1 > 4); every row is labelled `verdict_eligible=false`, so **no timing
number is claimed** from it — only the correctness columns (classes,
false_pass, infra_timeout, negative_correct). The 3 `step_timeout` rows are the
new taxonomy's own-deadline class (`negative-multi-wrong-after-back`,
`negative-offscreen-pager-wait`, …), not infra.

Acceptance corpus (`run-acceptance.sh`, groups core/proc/web/sysui/devices):
counter, form-validation, back-layers, back-layers-ime, gaten-backstack,
gaten-lesson, gaten-bubble, rel3s-gaten-scroll, a11y-probe, looperwake-bg-post,
touchdisp-dispatch-probe, touchdisp-gesture, touchdisp-posted-side-effect,
mproc3-two-process, mproc8-bind-only, binderpid, launch-extras,
launch-extras-parcelable (`status=unsupported`,
`UNSUPPORTED_ANDROID_API`, matching the scenario's `expect_error` — the
scenario's `expectation: unsupported` is what makes that a pass), webview-probe
×3, webview-fair ×3, sysui1-permission-allow/deny/pregranted, sysui1-shade,
notif-granted, notif-denied, location-attach/geocoder/detach,
sensors-scripted, sensors-off. All 34 pass.

Settle precedence (`host window removal > unhandled shortcut >
expect_change:false`) is untouched by all five merges: no side edited
`eval_tap_change`'s branch order.

## 4. Notes for the orchestrator

* `a11y-probe`/`counter` do **not** show GLSPIKE2's all-caps label (§2.3).
* The dangerous-permission rule (§2.1) is a **behaviour change** in a shared
  seam (`PermissionStore::check`): any future arm that expected "declared ⇒
  granted" for a dangerous permission now sees `DENIED` until granted. All
  suites and the four permission arms agree with it; it is the AOSP rule.
* `process_started()` keeps procuid's 4-arg shape, so any branch still calling
  the 3-arg form needs `, 0` (two call sites in `sysui4_result_ordering.rs`
  did).
