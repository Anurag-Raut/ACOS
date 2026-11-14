# GR4 handoff — Gaten screens past the merged five on real ART

**Status: INCOMPLETE. No GR4 scenario has a valid post-merge run. Nothing is committed.
No `results/gaten/GR4.md`, no `EXPERIMENTS.md` E13.10 entry, no Backlog rows yet.**

- Worktree: `/home/Qui/.paseo/worktrees/3oxa6xao/gr4`
- Branch: `gaten-gr4-screens` (off `main` `4151225` at task start). It was fast-forwarded
  to `d7dc8e2` when the density fix landed; there are **no branch commits** and the 8 new
  scenario files are **untracked**.
- APK: `/home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk`
  (the same APK the merged `gaten-*` flows use; GR3 sha256 `e04c7ffc…`).
- Nothing under `crates/`, `~/Projects/gaten`, `fixtures/`, or the phone was touched. No
  `adb`. No secrets read. No Gaten file changed.
- Scratch: `.cache/gr4/` (354 MB) and `.cache/gr4-target/` (1.1 GB), both gitignored.

---

## 1. Scope given, and what was found/decided

**Scope:** extend verified Gaten coverage beyond the five merged flows
(`gaten-home`, `gaten-tabs`, `gaten-settings`, `gaten-search`, `gaten-lesson`), write 3–5 new
strict scenarios plus deliberately wrong twins, record every screen ACOS cannot reach or
render as a wall (code, lowest ACOS layer, AOSP reference, bug vs missing feature), and
report. No fixes in `crates/`.

**Read first (done):** `results/gaten/GR3.md`, `GR1.md`/`GR2.md`, all
`scenarios/gaten-*.yaml` and `scenarios/negative/gaten-*-wrong.yaml`,
`EXPERIMENTS.md` E13.1–E13.7, `docs/AOSP_REFERENCE.md`, and the read-only Gaten tree
`/home/Qui/Projects/acos/.cache/gaten/src-dev`.

**Screen map decisions (from Gaten source and the GR3 captures):**

| Candidate | Decision | Why |
|---|---|---|
| Support sheet (Settings → Support Gaten) | **Chosen** (`gaten-support`) | A real RN `Modal` (`DialogShell`), reachable, with two dismissal paths (Close action and Android Back → `onRequestClose`). GR3 never exercised a dialog. |
| Settings → Kanji reference | **Chosen** (`gaten-kanji`) | A real radio group whose choice is persisted through the RTK bridge; the hub row's own value changes, so it is functional evidence like the merged Appearance theme test. |
| Settings → Text recognition → Enhanced | **Chosen** (`gaten-recognition`) | The download confirmation is a bounded `Modal` (`fitToViewport`), reachable without a network: tapping Enhanced only raises the dialog; Cancel must leave state untouched. GR3 did not cover this page or the dialog. |
| Analytics → Connect → WaniKani settings page → Back | **Chosen** (`gaten-backstack`, v2) | A settings page pushed on the **Analytics** stack and the Back out of it; proves the round trip returns to Analytics, not the Settings hub. |
| Tab-stack retention (word page → other tab → back) | **Ruled out / discarded** | The bottom bar is absent on every pushed page (Word, search results, Settings pages), and tab swipe is disabled when a stack has depth (`swipeEnabled: route.state.index === 0`). A first draft (`gaten-backstack` v1) failed 3/3 with `ELEMENT_OFFSCREEN`: the only `Analytics` content-desc node was the off-screen pager sibling at `[1138,193][1934,308]`, `visible_bounds <none>`. That is **correct fail-closed ACOS behaviour**, not a bug/wall; the scenario was rewritten (v2) instead. |
| Analytics drill-down (heatmap day / refresh) | **Not viable on this guest** | Day detail and the study heatmap need AnkiDroid data (`useAnkiStats`); with no AnkiDroid there is no grid, no selectable day. The refresh mark only proves the page stays up. Data wall, same class as GR3 §1, not an ACOS wall. |
| Lesson detail / completion / quiz | **Still unreachable** | Needs AnkiDroid (`com.ichi2.anki` not installed) or a stored WaniKani token. Same data wall as GR3; not fabricated. |
| Profile / account page | **Does not exist** | No standalone profile/account screen in Gaten source. The closest is the WaniKani settings page, now covered by `gaten-backstack` v2. |
| Rotate | **Ruled out** | Gaten is portrait-locked: `app/src/main/AndroidManifest.xml:112` `android:screenOrientation="portrait"` (comment at line 97: “Gaten's own UI is portrait only”). Rotating would only exercise the system, not a Gaten screen. |
| Anki cards settings page | **Not drafted** | Reachable blocked state (`Install Anki to add cards.`, `Allow Anki access to choose study decks.`), but it is another “not installed” page rather than a new interaction class. Good optional 5th if more coverage is wanted. |
| WaniKani invalid-token Save | **Not drafted** | Would hit the network/API and could turn a data failure into a misleading wall. Kept out. |

**Density fix:** the orchestrator said the SVG sizing fix landed on main (`00804dc`, then
`d7dc8e2`). `git merge --no-edit origin/main` was run and fast-forwarded the branch to
`d7dc8e2`; `acos-cli`/`acos-art` were rebuilt (9.4 s incremental, binary timestamps
09:44). **No scenario ran to completion after that merge**, so there are no post-fix
pixels and no size observations yet. Do not judge SVG/icon sizes from any pre-merge
capture; those artifacts were also deleted when `.cache/gr4/suite` was cleared before the
post-merge batch.

---

## 2. The 8 untracked scenario files

All are in the working tree, untracked (`git status --short` lists exactly these).
Run status is explicitly separated into **pre-merge** and **post-merge**.

| File | Steps | Targets / assertion spine | Run status |
|---|---:|---|---|
| `scenarios/gaten-support.yaml` | 27 | Home → Settings hub → tap `content_desc: "Support Gaten"` → modal body (“one-person project”, “a review or a coffee helps it keep going”), `Rate on Play Store`, `Close` → Close → body `assert_not_visible`, hub row `assert_visible` → reopen → `back: {}` → body `assert_not_visible`, hub still shows `Screen lookup` | **Pre-merge 3/3 pass** (`status=pass`, wall 8.5–8.7 s). **Post-merge: not run**; the killed batch left a 0-byte `pos-gaten-support-1.json` and a partial `art-pos-gaten-support-1/`. |
| `scenarios/negative/gaten-support-wrong.yaml` | 27 | Same flow; body asserted with wrong wording (`"a review or a subscription keeps it going"`) | **Never run.** |
| `scenarios/gaten-kanji.yaml` | 23 | Home → Settings hub → Kanji reference row (`content_desc contains "Kanji reference."`) → page (`Remembering the Kanji`, “bundled, works offline”, `WaniKani`) → tap RTK → `assert_not_visible` save-error line → Back → hub row text `Remembering the Kanji` | **Pre-merge 3/3 pass** (8.7–8.9 s). **Post-merge: not run.** |
| `scenarios/negative/gaten-kanji-wrong.yaml` | 23 | Same flow; after selecting RTK, the hub's `Remembering the Kanji` node is asserted to read `WaniKani` | **Never run.** |
| `scenarios/gaten-recognition.yaml` | 33 | Home → Settings hub → Text recognition → `Standard`/`Enhanced`/“Better for stylised fonts” → tap `content_desc contains "Enhanced"` → “Download Enhanced?”, `Download size`, reason, “Wi-Fi recommended”, `Cancel`, `Download` → Cancel → dialog `assert_not_visible`, page still shows `Enhanced` → Back → hub value `Standard` | **Pre-merge 3/3 pass** (9.5–9.9 s). **Post-merge: not run.** |
| `scenarios/negative/gaten-recognition-wrong.yaml` | 29 | Same flow; after Cancel, the hub's `Standard` node is asserted to read `Enhanced` | **Never run.** |
| `scenarios/gaten-backstack.yaml` | 24 | Home → Analytics tab → `Not connected` + “Connect to see WaniKani progress.” → tap `Connect` → WaniKani settings page pushed on the Analytics stack (token `content_desc "Personal access token"`, hint “Paste your token”; `New per day`; `Not connected`; `Save`) → Back → Analytics again (`Study activity`, `Accuracy`, heading `Analytics`) | **v1 (word-page tab switch) failed 3/3 pre-merge with `ELEMENT_OFFSCREEN`; v2 (current file) has never run.** |
| `scenarios/negative/gaten-backstack-wrong.yaml` | 24 | Same v2 flow; after Back the `Analytics` heading node is asserted to read `Settings` | **Never run.** |

**Accuracy of the “nothing committed / no GR4 report” suspicion: correct.**
`results/gaten/` contains only `G1.md`, `GR1*`, `GR2.md`, `GR3.md`, `W181A.md`, `evidence/`
— **no `GR4.md` and no `evidence/GR4/`**. `EXPERIMENTS.md` has no E13.8–E13.10 GR4 entry.
`git log` shows no GR4 commit.

### Scenario risks to smoke-test first

1. **`gaten-backstack` v2 (highest risk — never executed).** Selector assumptions:
   - Analytics' WaniKani `SourceUnavailable` action is the only visible `text: {exact: "Connect"}`
     (`AnalyticsScreen.tsx::waniKaniRead`, label “Connect” / “Connect again”).
   - The pushed page is `WaniKaniSettingsScreen` (`SettingsPageScaffold title="WaniKani"`,
     token field `accessibilityLabel="Personal access token"`, `FieldLabel label="New per day"`,
     `Save`).
   - `assert_text` on the token field passes via the EditText's `hint` (“Paste your token”,
     `eval_text` checks `node.hint`).
   - Back must land on Analytics; `wait_for "Study activity"` is strict and will report
     `TIMEOUT` (not `ASSERTION_FAILED`) if the push landed on another stack.
   - `assert_text text exact "Analytics"` may match both the tab-bar label and the heading;
     expected to pass via the visible heading, but confirm in the tree.
2. **`gaten-support` Back dismissal.** RN `Modal` should route Android Back to
   `onRequestClose`. If ACOS' `back` instead reaches the Activity/stack, the run fails in a
   non-assertion way; that would be a wall candidate (input routing / guest key handling),
   not a scenario bug. Close-action dismissal is already proven pre-merge.
3. **`gaten-recognition` hub assertion.** After Cancel, `selector text exact "Standard"` is
   expected to be the hub's Text-recognition value; make sure no second visible `Standard`
   node outranks it in `locate()`.
4. **Wrong twins must fail with `ASSERTION_FAILED` specifically.** Each twin deliberately
   selects a node that exists and asserts wrong text on it (not a missing selector, which
   would be `ELEMENT_NOT_FOUND`).
5. **Pre-merge results are stale** relative to `d7dc8e2`. They are useful as a behavioural
   sanity check only; the acceptance run must be post-merge.

---

## 3. Everything tried / ruled out (including informal work)

- **Gaten source survey** (`src-dev`, read-only) to choose reachable, assertable screens:
  `AnalyticsScreen.tsx`, `SettingsScreen.tsx`, `settings-components.tsx`,
  `KanjiReferenceSettingsScreen.tsx`, `RecognitionSettingsScreen.tsx`,
  `DownloadConfirmDialog.tsx`, `DialogShell.tsx`, `SupportSheet.tsx`, `SupportPrompt.tsx`,
  `WaniKaniSettingsScreen.tsx`, `AnkiSettingsScreen.tsx`, `AppShell.tsx`, `navigation.ts`,
  `AndroidManifest.xml`.
- **Analytics**: sections are not accordions (`AnalyticsSection` renders a static header +
  body), the only controls are the refresh mark and chart/heatmap selection; the content is
  data-gated on AnkiDroid/WaniKani. No useful new strict flow without data.
- **Modals inventory**: Support sheet, download confirmation, Anki “Discard changes?”,
  provider setup help, download-confirm in onboarding, WaniKani review dialog. The first
  two are reachable with no account; the others need an active session/edit path. Chose the
  first two.
- **Support prompt on Home** (`SupportPrompt`) is gated by a persisted eligibility/dismissal
  state, so it is not a deterministic first-run screen; Settings' own row is.
- **Back/tab semantics**: confirmed `openSettingsPage(navigation, page)` pushes
  `SettingsPage` on the *caller's* stack; `AnalyticsRoute` passes its own navigation, hence
  the v2 round trip. Confirmed the root tab bar disappears on pushed pages (GR3 screenshots:
  `gaten-search-word.png`, `gaten-search-results.png`, `gaten-settings-appearance.png`), and
  the pager's `swipeEnabled` is false once a tab stack has depth.
- **First `gaten-backstack` draft** tried `Home → search → 猫 word → Analytics tab`. It
  reached step 17 (`tap content_desc Analytics`) and failed closed with
  `ELEMENT_OFFSCREEN` for the off-screen pager sibling. Rewrote it rather than weakening
  visibility (no `include_invisible`, no fast assertion).
- **Environment setup** (done): `fixtures/build` copied from
  `/home/Qui/Projects/acos/fixtures/build`; `.cache/art`, `.cache/gaten`, `.cache/corpus`,
  `.cache/boot/android-34` symlinked; `ACOS_FONT_DIR` exported in the runner; `ACOS_BOOT_DIR`
  not set. Release build of only `-p acos-cli -p acos-art` into `CARGO_TARGET_DIR=$PWD/.cache/gr4-target`.
- **Runner** (`.cache/gr4/run_suite.sh`, gitignored): wraps `acos verify <scenario> --apk <APK>
  --engine art --json --artifacts .cache/gr4/suite/art-<tag>`; modes `newpos <runs>`,
  `newneg <runs>`, `old`, `oldneg`, `one <name>`; per-run JSON/err under `.cache/gr4/suite/`.
  Note: its `verdict()` helper reads the wrong JSON path and prints `steps=0`; the real
  per-step data is under `result.verdict.steps` (or `.result.steps`); fix before trusting
  step counts. Exit status is still read from the JSON `status`/`error.code`.
- **ART lock behaviour**: the host lock is heavily contended (queue waits of 3–6 min were
  normal). `run-exclusive.sh` starts its elapsed timer only after `flock`; a shell
  `timeout` around the whole invocation must be much larger than `guarded --time`, or the
  wrapper is killed while still queued (this happened once on the first smoke run; the run
  itself had actually completed and passed).
- **No ACOS wall was proven in this task.** The only failure observed was
  `ELEMENT_OFFSCREEN` and it was a correct refusal to tap/report a node from an off-screen
  pager sibling; the app intentionally hides the tab bar on pushed pages. Existing data
  walls from GR3 (AnkiDroid absent, no WaniKani token) still stand.

---

## 4. Exact next actions, in order

Run everything from `/home/Qui/.paseo/worktrees/3oxa6xao/gr4`.

1. **Sanity-check the tree and toolchain.**
   ```bash
   git status --short            # expect exactly the 8 untracked scenario files
   git log --oneline -1          # expect d7dc8e2 (density fix included)
   ls .cache/gr4-target/release/acos .cache/gr4-target/release/acos-art
   ```
   If the binary is missing, rebuild:
   ```bash
   export CARGO_TARGET_DIR=$PWD/.cache/gr4-target
   cargo build --release --offline -p acos-cli -p acos-art
   ```
   If `fixtures/build` is missing/empty:
   ```bash
   rm -rf fixtures/build && cp -a /home/Qui/Projects/acos/fixtures/build fixtures/
   ```
   Runner env is already inside `.cache/gr4/run_suite.sh` (`ACOS_FONT_DIR`,
   `ACOS_ART_BIN`, `ACOS_ART_DATA`; no `ACOS_BOOT_DIR`).

2. **Smoke the rewritten backstack scenario alone** (it has never run), and optionally the
   support modal immediately after:
   ```bash
   timeout 1200 tools/art-runtime/run-exclusive.sh --task gr4 -- \
     tools/guarded.sh --mem 8G --time 180 -- bash .cache/gr4/run_suite.sh one gaten-backstack
   cat .cache/gr4/suite/one-gaten-backstack.json | python3 -m json.tool | head -40
   ```
   The run writes `.../one-gaten-backstack.json` and `art-one-gaten-backstack/`. If it fails
   before an assertion (`TIMEOUT`/`ELEMENT_OFFSCREEN`/`ELEMENT_NOT_FOUND`), inspect the
   per-step JSON and `tree/` and fix the selector/label, not the visibility rule. The most
   likely selectors to adjust: `Connect`, `New per day`, `Personal access token`,
   `Analytics`.

3. **Run the new positives 3× each** (one lock hold; ~12 runs × ~10 s):
   ```bash
   timeout 1200 tools/art-runtime/run-exclusive.sh --task gr4 -- \
     tools/guarded.sh --mem 8G --time 570 -- bash .cache/gr4/run_suite.sh newpos 3
   ```
   Expect 12/12 `status=pass`. Any `fail` is a scenario/app wall to classify, not a pass.

4. **Run the new negatives 3× each** (12 runs):
   ```bash
   timeout 1200 tools/art-runtime/run-exclusive.sh --task gr4 -- \
     tools/guarded.sh --mem 8G --time 570 -- bash .cache/gr4/run_suite.sh newneg 3
   ```
   Accept only `status=fail` with `error.code=ASSERTION_FAILED`; each must be 3/3. Anything
   else (`TIMEOUT`, `ELEMENT_*`, `GUEST_EXITED`) is not the required control.

5. **Regression: the merged five flows + five negatives** (10 runs):
   ```bash
   timeout 1200 tools/art-runtime/run-exclusive.sh --task gr4 -- \
     tools/guarded.sh --mem 8G --time 570 -- bash .cache/gr4/run_suite.sh old
   timeout 1200 tools/art-runtime/run-exclusive.sh --task gr4 -- \
     tools/guarded.sh --mem 8G --time 570 -- bash .cache/gr4/run_suite.sh oldneg
   ```
   Compare against GR3: positives must pass (the known R2a SIGSEGV is a fail-closed
   `GUEST_EXITED`, never a pass); negatives must fail `ASSERTION_FAILED`.

6. **Judge pixels now that the density fix is in.** The orchestrator explicitly cleared
   SVG/icon-size judgement after `d7dc8e2`; do not file the still-unmatched Moto G35 exact
   pixel parity as a wall. Capture evidence PNGs from
   `.cache/gr4/suite/art-pos-*/frames/` (downscale, do not commit floods) into
   `results/gaten/evidence/GR4/`.

7. **Write the deliverables (none exist yet):**
   - `results/gaten/GR4.md`: what ran, the scenario table `(scenario, steps, result ×3,
     wall p50, wall if any)`, walls with exact code + lowest ACOS layer + AOSP reference
     (`/home/Qui/Projects/acos/.cache/aosp/`) + ACOS bug vs missing feature, and explicit
     “0 false passes”.
   - `EXPERIMENTS.md` **E13.10** entry (pass or fail), matching the style of E13.7.
   - **Backlog rows `G4a`…** for any wall found. If `gaten-support` Back dismissal fails,
     the row is likely input/key routing in the ART launcher or RN Modal handling; name the
     lowest layer from the trace, not a guess.
   - Then commit on `gaten-gr4-screens` (orchestrator authorised commits on this branch).
     **Do not push, do not merge.**

8. **Cleanup/limits:** `.cache/gr4` is 354 MB and `.cache/gr4-target` 1.1 GB (within 5 GB).
   Do not add per-run PNG floods; keep the default screen cap. Keep each lock hold ≤10 min;
   batch as above.

---

## 5. Blockers, risks, open questions for the orchestrator

1. **The post-merge acceptance is entirely unrun.** The last ART batch was interrupted by
   the handoff at `pos-gaten-support-1`; its JSON is 0 bytes. Treat every “pass” in §2 as
   pre-merge behaviour only.
2. **`gaten-backstack` v2 is unproven.** It encodes a real product behaviour (`Connect`
   pushes settings on the Analytics stack; Back returns there). If the labels differ in the
   f344272 APK, only the scenario needs adjustment — the behaviour claim must still be
   proven, not weakened.
3. **Back-to-dismiss modal is an open question.** If Android Back is not delivered to the RN
   Modal by ACOS, classify the lowest layer before filing: scenario → executor input →
   `acos-art` key injection → guest `Activity.dispatchKeyEvent`/RN `ModalHostView`. AOSP
   reference for the last step:
   `.cache/aosp/base/core/java/android/app/Activity.java` (`onBackPressed`/`dispatchKeyEvent`)
   and `.cache/aosp/base/core/java/android/view/KeyEvent.java`.
4. **Step-count/reporting helper bug:** `.cache/gr4/run_suite.sh`'s `verdict()` prints
   `steps=0` because it looks at `d.verdict.steps`, while the runner emits
   `d.result.verdict.steps`. Fix before producing the GR4 table, or count steps from the YAML.
5. **E13 numbering:** GR3 used E13.7. The task asks for **E13.10**; check the merged
   `EXPERIMENTS.md` for E13.8/E13.9 (other agents may have added them) and use E13.10 only
   if it is still free.
6. **No wall list to hand over.** The only observed failure was a correct
   `ELEMENT_OFFSCREEN` refusal; the GR3 data walls (AnkiDroid absent, no WaniKani token)
   remain unchanged. If the density fix changes a layout assertion, judge it as a rendering
   issue now, but do not file the Moto G35 exact-parity gap.
7. **Branch state:** `gaten-gr4-screens` currently equals `origin/main` `d7dc8e2` (the merge
   was a fast-forward); the 8 scenarios are the only local changes. A fresh engineer can
   commit them directly without reconstructing anything.
