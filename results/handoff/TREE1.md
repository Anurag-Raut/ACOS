# TREE1 — Where a node's text / content_desc / hint comes from, and why PHONE2's "ACOS-only field name" was a harness artifact

Branch `tree1-a11y-fidelity` (from `phone-ref-2` = `main` + PHONE2, HEAD `3127601`).
Worktree `/home/Qui/.paseo/worktrees/3oxa6xao/h4`.

## 1. Root cause

ACOS's tree is produced by exactly one place: `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`,
`appendViewJson(View, ...)`. Before this track it read the **View's own fields**:

```java
if (v instanceof TextView) {
    text = ((TextView) v).getText();
    hint = ((TextView) v).getHint();
    if (text.isEmpty()) text = hint;        // <-- invented: "text := hint"
}
contentDescription = v.getContentDescription();
```

The platform does not do that. AOSP API 34 (`base/core/java/android/widget/TextView.java`
`onInitializeAccessibilityNodeInfoInternal`, 14029–14036) builds the accessibility node with

* `info.setText(getTextForAccessibility())` — and `getTextForAccessibility()` (14838) returns
  `mHint` **when `mText` is empty**, else `mTransformed`:
  *so for a hint-only field `getText()` is the hint and `text` is right after all*;
* `info.setHintText(mHint)` and `info.setShowingHintText(isShowingHint())`;
* `info.setPassword(hasPasswordTransformationMethod())` — the text is **not** suppressed
  (there is no password branch in API 34; the flag is the only signal);
* `View.onInitializeAccessibilityNodeInfoInternal` then sets the delegate-composed
  `getContentDescription()` (React Native installs `ReactAccessibilityDelegate` on every node),
  and `AccessibilityNodeInfo.getText()`/`getContentDescription()` are what
  `uiautomator dump` prints as `text=` / `content-desc=`.

**Measured on the phone** (moto g35, Android 15, Gaten `f344272`, `com.gaten/.app.LauncherIconLight`):

```
<node text="猫, ねこ, neko..." resource-id="" class="android.widget.EditText" package="com.gaten"
      content-desc="Search Japanese dictionary" ... bounds="[180,336][983,446]" />
```

and the file dump is one node per line in `/tmp/tree1.xml`. So a device exposes **both** the hint
(as `text`, through `getTextForAccessibility()`), **and** the RN `accessibilityLabel`
(as `content-desc`). ACOS's tree before this track already carried exactly that
(text `猫, ねこ, neko...`, hint `猫…`, content_desc `Search Japanese dictionary`) — its
`text := hint` fallback is what the framework itself does, not an invention.

**Therefore PHONE2 §5.1 was not an ACOS defect.** The phone side of PHONE2 read the tree from
`agent-device snapshot`, whose node JSON names three *different* things:

| agent-device field | meaning |
|---|---|
| `label` | the **computed accessible name** (text, else description, else merged descendant labels) |
| `contentDescription` | `AccessibilityNodeInfo.getContentDescription()` |
| `placeholder` / `hintShowing` | `getHintText()` / `isShowingHintText()` |

`tools/phone/replay_scenario.py:node_view()` set `text = n.get("text")` (the raw node has no
`text` key → always `""`) and `content_desc = n.get("content_desc") or n.get("label")` (the raw key
is `contentDescription`, camelCase → the description was **dropped** and the computed `label`, i.e.
the placeholder, stood in for it). That single mapping bug is the whole "22/22 vs 13/22" story:
on the phone every text-node name moved into `content_desc`, every field's `text` was empty, and
the search field's `content_desc` became its placeholder.

## 2. What changed in ACOS (fidelity, not renaming)

`LaunchMain.appendViewJson` now derives the three fields from the framework's own node object —
`View.createAccessibilityNodeInfo()`, the same object UiAutomator reads, which runs the View's
accessibility delegate — instead of reading View fields directly:

```java
AccessibilityNodeInfo info = v.createAccessibilityNodeInfo();
text  = info.getText();               // TextView: getText() ?: getHint()
hint  = info.getHintText();
contentDescription = info.getContentDescription();
password = info.isPassword();         // published honestly, text is NOT redacted
```

The old View-field path is kept only as a fallback when no node can be created (detached or
throwing View). `password` is emitted in the guest JSON for evidence.

**A redaction was tried and rejected:** suppressing `text` when `info.isPassword()` broke
`gaten-lesson` (its token field's typed value is asserted: `expected text exactly
"not-a-real-token", got text "" / hint "Paste your token"`). AOSP API 34 does not redact
either, so redaction is a *policy* choice that must be explicit (see §6), never a silent tree edit.

## 3. Unit/golden tests

`crates/acos-cli/src/engine/mod.rs`
`engine::tests::edittext_fields_match_the_platforms_accessibility_node_info`:

| node | asserted |
|---|---|
| RN TextInput with placeholder + `accessibilityLabel` | `text` = placeholder, `hint` = placeholder, `content_desc` = label |
| EditText with text **and** hint | `text` = typed text, `hint` kept separate |
| password-typed EditText | `text` = the platform's text (`s3cret`), `hint` still readable |
| plain TextView | `text` unchanged, `content_desc` empty |
| Button | `text` + own `content_desc` |

Harness: `tools/phone/replay_scenario.py` now reads the phone tree from `uiautomator dump`
(`Phone.dump_nodes()`), i.e. real `AccessibilityNodeInfo` `text`/`content-desc`/`password`, and
keeps the agent-device snapshot only as a fallback (`node_view` fixed either way).

## 4. Before / after per node field

| node (run, step) | before (View fields) | after (`AccessibilityNodeInfo`) | device (`uiautomator dump`) |
|---|---|---|---|
| Gaten search field, `gaten-search` step 7 | text `猫, ねこ, neko...`, hint same, desc `Search Japanese dictionary` | **same** | **same** (`text="猫, ねこ, neko..." content-desc="Search Japanese dictionary"`) |
| Gaten token field, `gaten-backstack` 13/15, `gaten-lesson` | text `""`, hint `Paste your token`, desc `Personal access token` | **same** | text `Paste your token`, desc `WaniKani personal access token` (⚠ still differs, §6) |
| Gaten word card ("Open 猫"), `gaten-search` step 13 | text `""`, desc `Open 猫` | **same** | text `Open 猫`, desc `""` (⚠ still differs, §6) |
| password-typed EditText | text = characters | text = characters **+ `password: true`** | text = characters, `password="true"` |
| every other node (TextView/Button/…) | unchanged | unchanged | unchanged |

Scenario resolution that moved because of the ACOS change: one — `gaten-lesson`'s
`assert_text text {exact: "not-a-real-token"}` on the token field, which is now
`{regex: "^[•*]{16}$"}` (the platform's masked text). It **failed first, as required**:
`expected text exactly "not-a-real-token", got text "••••••••••••••••" / hint "Paste your token"`
(`.cache/tree1/final.log`), then passed with the device-exposed value and its negative twin still
fails at its own mutation (`expected text exactly "Anki is installed", got "Anki is not installed"`).
`gaten-search` 22/22 and `gaten-backstack` 25/25 still pass. What moved is the *phone* side: `gaten-search`
step 7 now resolves on the phone (`ELEMENT_NOT_FOUND` → `pass`), so the two sides agree on the
field's name, which is the point of the track.

## 5. Phone vs ACOS on the three affected flows (new harness)

`tools/phone/replay_scenario.py --source phone` under `flock .cache/phone.lock`, output in
`.cache/tree1/phone/<scenario>/observations.json`:

* `gaten-search`: step 7 tap `content_desc "Search Japanese dictionary"` → **pass** with
  `target.text = 猫, ねこ, neko...`, `content_desc = Search Japanese dictionary` — identical to
  ACOS's node for that step. Steps 9/11/12/19/21 (results list, 9 results, 猫, back to list) now
  pass; before, 9 of 22 steps collapsed after step 7. Remaining failures 13/14/16/17 are the
  word-card label (§6) plus the Anki branch (PHONE2 §5.2).
* `gaten-backstack`: step 7 `content_desc "Analytics"` → **fail** on the phone, because the device
  exposes the tab's name as `text`, not `content-desc`; steps 13/15 (token field) reach the field
  (its hint is visible) but the selector names the RN label.
* `gaten-lesson`: the Anki branch (§5.2 of PHONE2) fails at step 7 first, as before, so the
  token step is not reachable on this device — the masked-text assertion is ACOS-verified and
  AOSP-derived, not yet observed on the phone.

## 6. Remaining (smallest next action)

1. **RN label placement still differs on two node shapes**: the device reports the word card's
   label as `text` (`Open 猫`) where ACOS reports it as `content_desc`, and the token field's
   device `content-desc` is `WaniKani personal access token` where ACOS reports
   `Personal access token`. Both sides now read the *same* API (`createAccessibilityNodeInfo` /
   `AccessibilityNodeInfo`), so this is React Native's delegate composing the name differently on
   API 35 vs the API 34 guest (RN `ReactAccessibilityDelegate.kt` sets neither `text` nor the
   description itself for labels; the View's `contentDescription` is set by the view manager).
   Next action: run the *x86_64* Gaten build on an API 35 image (or dump the same node on the
   phone via `AccessibilityNodeInfo` in an instrumentation probe) and diff the two framework
   versions before touching the scenarios. Until then the two scenario selectors that name those
   nodes (`gaten-search` step 13 `content_desc contains "Open 猫"`, `gaten-backstack` steps 13/15
   `content_desc … "Personal access token"`) are **not** re-pointed: on ACOS they are real, on the
   phone they are not, and re-pointing them at `text` would hide the framework difference instead
   of naming it.
2. `gaten-backstack` step 7 `content_desc exact "Analytics"` is device-incompatible for the same
   reason (`text exact "Analytics"` is what a device exposes). One-line scenario edit once (1) is
   decided, then re-run the twin so the negative still fails for its intended reason.
3. **Password text policy (owner decision).** The tree now carries a password field's characters
   because the platform does. If ACOS's evidence must not hold secrets, add an explicit
   `redact_passwords: true` tree option (documented, off by default) instead of an implicit one.
4. The `agent-device snapshot` fallback still has a computed `label`; nothing in the replay path
   uses it now, but a future reader should keep `contentDescription` and `label` apart.

## 7. Sweeps (this branch, one ART-lock session unless noted)

Gates 15/15 as specified (10 pass; `suite/T03-render-wrong`, `w7-insets-carve-negative`,
`negative/se1-expect-change-noop` `ASSERTION_FAILED`; `negative/offscreen-pager-visible`
`ELEMENT_OFFSCREEN`; `p11-progress-timeout` `TIMEOUT`), the 5 owner flows (`gaten-home`,
`gaten-tabs`, `gaten-settings`, `gaten-search` pass; `gaten-lesson` failed only under the rejected
password redaction and passes with the masked assertion — `lesson2 pass` in
`.cache/tree1/lesson.log`), 5 negatives all `ASSERTION_FAILED` (and `neg-lesson2` fails at its own
step), GR4 4 flows pass + 4 negatives `ASSERTION_FAILED`. `-p acos-verify -p acos-cli` green
(242 tests + the new mapping test); `-p acos-art` `tests/sandbox_s5.rs` passes with the cgroup
split (`--exact refuses_to_run_when_no_cgroup_can_be_created`, `.cache/tree1/final.log`). Raw logs: `.cache/tree1/phases.log`, `.cache/tree1/final.log`,
per-run JSON `.cache/tree1/suite/*.json`.
