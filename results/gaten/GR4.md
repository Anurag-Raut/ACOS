# GR4 — Gaten screens past the merged five: modals, a second Settings page, and a tab-stack Back

Implementer task, continuation of `results/handoff/GR4.md`. Worktree
`/home/Qui/.paseo/worktrees/3oxa6xao/gr4`, branch `gaten-gr4-screens`, off `main` `d7dc8e2`
(includes the density fix `00804dc`). APK read-only:
`/home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk`
(sha256 `e04c7ffcf01209c98e511e1ab01a7f68d6decb178d14a08f2b7f0a6539e2ee29`). No Gaten source,
bundle, asset or APK was modified; nothing in `~/Projects/gaten` was touched, checked out, built or
run. No secrets read. `crates/` unchanged — this task adds scenarios and evidence only.

**One-line verdict: four new Gaten scenarios verify strictly on real ART after the density fix —
3/3 each, 34 runs in total with **0 false passes** — a RN `Modal` (support sheet) opened and
dismissed by both the Close action and Android Back, a second Settings page whose choice persists
through the RTK bridge (Kanji reference), a second `fitToViewport` modal whose Cancel leaves state
untouched (Text recognition → Enhanced), and a Settings page pushed on the **Analytics** tab's own
stack whose Back returns to Analytics. All four deliberately-wrong twins fail with
`ASSERTION_FAILED` (not `TIMEOUT`/`ELEMENT_*`), and the five merged flows plus their five negative
controls still pass/fail as GR3 recorded.**

## 0. What changed in ACOS

Nothing under `crates/`, `fixtures/` or `docs/`: no assertion, visibility, settle or capture rule
was touched, and no gate moved. Three things changed outside the product:

| file | change | why |
|---|---|---|
| `scenarios/gaten-backstack.yaml` | last step re-targeted | the draft asserted `text exact "Analytics"`, which on this app is a **tab-bar label** (`Text`, `visible=false`) and not the page heading (the heading is `content_desc: "Analytics"` with empty text). The strict run reported `ELEMENT_OFFSCREEN` on the label. Replaced with Analytics-only copy (`Accuracy`, `Answer accuracy will appear here.`) plus `assert_not_visible` on the Settings hub's `Kanji reference` row, so a Back that landed on Home or the Settings hub cannot satisfy it. The claim is unchanged and no rule was weakened; see §2.2. |
| `scenarios/negative/gaten-backstack-wrong.yaml` | last step re-targeted | same reason, and it must fail `ASSERTION_FAILED`, not `ELEMENT_OFFSCREEN`. It now asserts the visible heading (`content_desc: "Analytics"`) reads `Settings`. |
| `.cache/gr4/verdict.py` (gitignored scratch) | new | the handoff's runner read `d["verdict"]["steps"]` and always printed `steps=0`; the real path is `result.verdict.steps` and step entries carry `status`, not `ok`. Step counts and failure codes in the tables below come from the corrected parser. |

The four positive scenarios and the other three twins were **not** edited from the handoff's draft,
so the earlier pre-merge 3/3 runs of `gaten-support`/`gaten-kanji`/`gaten-recognition` stand as
behavioural sanity checks and all acceptance numbers below are post-merge (`d7dc8e2`).

## 1. Scope

New coverage added past the merged five (`gaten-home`, `gaten-tabs`, `gaten-settings`,
`gaten-search`, `gaten-lesson`):

| new scenario | interaction class it adds |
|---|---|
| `gaten-support` | a React Native `Modal` (`DialogShell`): opened from the Settings hub, dismissed by the **Close** action, reopened, dismissed by **Android Back** (`onRequestClose`) |
| `gaten-kanji` | a pushed Settings page whose radio choice is **persisted** through the RTK bridge — the hub row's own value changes (`Remembering the Kanji`) |
| `gaten-recognition` | a bounded `fitToViewport` confirmation modal (Text recognition → Enhanced): `Download` is *not* pressed, `Cancel` must leave the hub value at `Standard` |
| `gaten-backstack` | a Settings page pushed on **the Analytics tab's own stack** and the Back out of it — the round trip returns to Analytics, not to the Settings hub or Home |

Each has a twin in `scenarios/negative/`.

## 2. Scenarios — post-merge acceptance runs

All runs: `acos verify <scenario> --apk <APK> --engine art --json --artifacts .cache/gr4/suite/…`,
inside one ART-lock acquisition per batch
(`tools/art-runtime/run-exclusive.sh --task gr4 -- tools/guarded.sh --mem 8G --time …`),
`animations: off`. Common opening is GR3's: one strict tap on “Set up Gaten”, three
`settle: fast` Skip taps (navigation only, exactly as GR3 does), then a strict Home wait; every claim
in the table is a `wait_for`/`assert_*` on a bound capture frame, never on a fast tap's reply.

| scenario | steps | runs | result | failure class | wall p50 | evidence |
|---|---:|---:|---|---|---:|---|
| `scenarios/gaten-support.yaml` | 27 | 3 | **3/3 pass** | — | 8.40 s | `evidence/GR4/gaten-support-{home,hub,sheet,closed,sheet-again,back-dismissed}.png` |
| `scenarios/gaten-kanji.yaml` | 23 | 3 | **3/3 pass** | — | 8.66 s | `evidence/GR4/gaten-kanji-{hub,page,selected,hub-after}.png` |
| `scenarios/gaten-recognition.yaml` | 33 | 3 | **3/3 pass** | — | 9.58 s | `evidence/GR4/gaten-recognition-{page,confirm,cancelled,hub-after}.png` |
| `scenarios/gaten-backstack.yaml` | 25 | 3 (+1 smoke) | **4/4 pass** | — | 8.34 s | `evidence/GR4/gaten-backstack-{analytics,wanikani-page,analytics-again}.png` |

Merged five, re-run post-merge (one run each, the GR3 acceptance shape):

| scenario | steps | result | wall | evidence |
|---|---:|---|---:|---|
| `gaten-home` | 14 | **pass** | 5.88 s | `.cache/gr4/suite/old-gaten-home-1.json` |
| `gaten-tabs` | 22 | **pass** | 8.86 s | `…/old-gaten-tabs-1.json` |
| `gaten-settings` | 33 | **pass** | 11.32 s | `…/old-gaten-settings-1.json` |
| `gaten-search` | 22 | **pass** | 8.38 s | `…/old-gaten-search-1.json` |
| `gaten-lesson` | 28 | **pass** | 10.20 s | `…/old-gaten-lesson-1.json` |

Every one of the 34 runs ended with `session_exit.outcome = ok`, `guest_exit = 0`, no
`native_crash`, and a guest cgroup peak of **236–429 MiB** (budget `memory_mib: 4096`). The known
R2a/G3d launch SIGSEGV did not recur (0 of 34 launches since `d7dc8e2`).

### 2.1 The four new claims, and how each is proven strictly

- **`gaten-support`** — the modal's body text (`Gaten is a one-person project…`), the literal
  **Rate on Play Store** action and the **Close** action are asserted while the sheet is up; after
  Close the same body selector has **no visible node** (`assert_not_visible`) and the Settings hub
  row `Support Gaten` is visible again. Reopened, then `back: {}`: the body is gone **and** the hub
  is intact. So ACOS delivers Android Back to the RN `Modal`'s `onRequestClose` — the open question
  the handoff listed as risk 2 is answered: **not a wall, works**.
- **`gaten-kanji`** — the pushed page asserts `Remembering the Kanji`, “bundled, works offline” and
  `WaniKani`; after tapping RTK the page's save-error line has no visible node, and **after Back the
  hub's own Kanji reference row reads `Remembering the Kanji`**. That is functional persistence, the
  same evidence class as GR3's Theme Light → Dark.
- **`gaten-recognition`** — the page asserts `Standard` / `Enhanced` / “Better for stylised fonts”;
  tapping Enhanced raises the confirmation (`Download Enhanced?`, `Download size`, the reason copy,
  `Wi-Fi recommended`, `Cancel`, `Download`); **Cancel** removes it (`assert_not_visible`) and the
  page still shows `Enhanced` unselected, and **after Back the hub's Text recognition row still
  reads `Standard`** — i.e. cancelling a download confirmation changes no persisted state.
- **`gaten-backstack`** — Analytics with no token shows `Not connected` +
  “Connect to see WaniKani progress.”; tapping that section's **Connect** pushes the WaniKani
  settings page **on the Analytics stack** (token field `content_desc "Personal access token"` with
  hint “Paste your token”, `New per day`, `Not connected`, `Save`); `back: {}` returns to the
  Analytics dashboard (`Study activity` waited strictly, `Accuracy`, “Answer accuracy will appear
  here.”), and the Settings hub's `Kanji reference` row is not visible. No bottom tab bar is used
  after the push, so the claim holds whether or not the app hides it.

### 2.2 Why the last `gaten-backstack` step was re-targeted (not weakened)

The handoff's v2 draft ended with `assert_text: {selector: {text: {exact: "Analytics"}}, …}`. Its
first strict run failed at that step with `ELEMENT_OFFSCREEN` while **every earlier step passed**,
including the Back:

```
step 23 (assert_text) fail: selector … TextMatch { exact: "Analytics" } … matched no visible node,
but it matches a node that is not visible to the user (text "Analytics" class X4.e
bounds [286,2151][416,2188] visible_bounds [286,2151][416,2188] in window "main"))
```

`tree/18.json` (the same bound frame) shows why: the string `Analytics` appears as (a) the page
heading, `content_desc "Analytics"` / `text ""`, `[58,193][854,308]` **visible**, (b) the bottom-bar
tab **labels**, `Text` nodes with `visible=false` at `y 2151` and `y 2214`, and (c) the tab
**pressables** (`content_desc "Analytics"`, visible). The `text:` selector can only ever match (b).
So the selector — not the assertion — was wrong: the draft asserted a node the app never paints.
Nothing was relaxed: the scenario still requires the Analytics dashboard after Back and now also
requires the Settings hub row to be **absent**, which a Back that overshot to the hub or to Home
cannot satisfy. See Backlog `G4a` for the tree/frame inconsistency this exposed.

## 3. Negative controls — the same flows, deliberately wrong

Each twin selects a node that exists and is **visible**, then asserts wrong text on it, so the
failure class is `ASSERTION_FAILED` and never `ELEMENT_NOT_FOUND`/`ELEMENT_OFFSCREEN`/`TIMEOUT`.

| negative | steps | runs | result | failing step | message |
|---|---:|---:|---|---|---|
| `scenarios/negative/gaten-support-wrong.yaml` | 14/27 | 3 | **3/3 `ASSERTION_FAILED`** | 13 | expected text containing “a review or a subscription keeps it going”, got “Gaten is a one-person project. If it has been useful, a review or a coffee helps…” |
| `scenarios/negative/gaten-kanji-wrong.yaml` | 23/23 | 3 | **3/3 `ASSERTION_FAILED`** | 22 | expected text exactly “WaniKani”, got “Remembering the Kanji” |
| `scenarios/negative/gaten-recognition-wrong.yaml` | 29/29 | 3 | **3/3 `ASSERTION_FAILED`** | 28 | expected text exactly “Enhanced”, got “Standard” |
| `scenarios/negative/gaten-backstack-wrong.yaml` | 24/24 | 3 | **3/3 `ASSERTION_FAILED`** | 23 | expected text exactly “Settings”, got “” (the Analytics heading, whose text is empty) |

Merged five negatives, re-run post-merge (one run each): `gaten-home-wrong` 8/8,
`gaten-tabs-wrong` 8/8, `gaten-settings-wrong` 13/13, `gaten-search-wrong` 17/17,
`gaten-lesson-wrong` 11/11 — **5/5 `ASSERTION_FAILED`**, same steps after which the positive stops
being able to satisfy the flow.

**0 false passes**: every positive verdict is `pass` with a strict spine, every negative verdict is
`fail` with `error.code = ASSERTION_FAILED`. No scenario was forced green, and no assertion,
visibility rule or settle horizon was changed to make anything pass.

## 4. Post-merge pixels: the density fix in the evidence

The orchestrator cleared SVG/icon-size judgement until `d7dc8e2`; every capture below is from that
commit. **The 2.625× SVG regression is gone.**

- **Visual A/B, same screen, same APK.** `evidence/GR3/gaten-tabs-analytics.png` (pre-fix) draws the
  header wordmark and the Anki/WaniKani illustrations roughly 2.6× too large and clipped at the top
  of the frame, with the section labels overlapped by the artwork. The GR4 file
  `evidence/GR4/gaten-backstack-analytics-again.png` (post-fix) draws the same screen with the
  heading, the Anki card, the WaniKani torii, the 12-week heatmap and the accuracy ring all inside
  their own boxes, text unobstructed.
- **Measured boxes (post-fix trees, display px, `density_dpi: 400`).** Every wordmark glyph
  `SvgView` is exactly **116 px** tall (e.g. `[58,188][105,304]` = 47×116; the whole 8-glyph
  wordmark `[58,188][391,304]` = 333×116) — GR1's expected viewBox height for this asset was
  116 px, and a 2.625× double-scale would have produced ~305 px. Settings hub leading icons are
  58×57 px, rows' chevrons 39×39 px, the support sheet's heart/star/coffee icons 52×53 px, bottom
  bar icons 52×53 px. Nothing overflows its parent row (`[58,431][1022,588]` etc.).
- **The modal renders as a real modal**: `DialogShell` draws as a rounded card over a dimmed
  Settings page (`gaten-support-sheet.png`), and the download confirmation
  (`gaten-recognition-confirm.png`) is a bounded centred card — both are composed by real ART, not
  a preview.
- **No exact-parity claim is made** against the Moto G35; the two open items are in §5 and the
  Backlog (`G4a`, `G4b`), not a parity wall.

## 5. Walls

**No new blocking ACOS wall, and no data wall moved.** The GR3 data walls still stand unchanged:
AnkiDroid (`com.ichi2.anki`) is not installed and no WaniKani token is stored, so Anki decks/cards
and the WaniKani lesson/review queue remain unreachable — GR4 does not fabricate them (it proves the
gates and the connection forms instead). Two **observations** were recorded rather than fixed,
because neither blocks a scenario:

- **`G4a` — `visible=false` on a node whose pixels the app painted.** In `tree/18.json` the bottom
  bar's active label is `[512,2214][642,2251]`, `visible: false`, below the root's
  `visible_bounds` bottom (`2209`); the bound frame `frames/18.png` paints that label's glyphs at
  png `y 2088–2122`, `x 513–640` — exactly those bounds minus `display_origin.y = 128`. So
  ACOS classifies a node invisible that the app drew and the frame contains (the run is
  `insets: carve`, so the bottom 128 px of the app window is outside the "visible to the user"
  band). Lowest ACOS layer: the tree's per-node `visible`/`visible_bounds` computation against the
  carved visible frame, surfaced by the executor as `ELEMENT_OFFSCREEN`
  (`crates/acos-verify/src/native.rs` locate → `ErrorCode::ElementOffscreen`). It is a
  **verification false-negative**, never a false pass; AOSP's `View.getGlobalVisibleRect` intersects
  with the display and ancestor clips, not with the navigation-bar band, so a real accessibility
  tree would still report the node visible (while a tap there would land on the system bar —
  hence "fail closed" is defensible and the *classification* is what is inconsistent). Practical
  rule for scenario authors, applied here: **target the tab pressable's `content_desc`, never the
  bottom-bar label text.**
- **`G4b` — tree frame metadata reports `density: 420` for a `density_dpi: 400` run.**
  `repro.json` and `verdict.json` echo `400` (`display: {mode: offscreen, width: 1080,
  height: 2400, density_dpi: 400}`), while every `tree/*.json` frame record says `420`. Either the
  guest really boots at the reference 420 on this path, or the tree's `FrameMeta` is stamped from
  the reference default. Not root-caused here. Consequence: density-dependent geometry cannot be
  audited from the tree metadata alone, which matters for exactly the work the density fix belongs
  to. The pixels are correct either way (§4).

## 6. Gates and tests

No crate, fixture or doc under `docs/` was modified, so no ACOS gate changed: the acceptance here
*is* the 34 ART runs. `acos-cli`/`acos-art` were rebuilt against `d7dc8e2`
(`CARGO_TARGET_DIR=$WT/.cache/gr4-target`, `cargo build --release --offline -p acos-cli -p acos-art`
→ “Finished … in 0.11s”, i.e. already current; binaries 2026-11-13 09:44).

## 7. Files

- New scenarios: `scenarios/gaten-{support,kanji,recognition,backstack}.yaml` and
  `scenarios/negative/gaten-{support,kanji,recognition,backstack}-wrong.yaml`.
- Evidence: `results/gaten/evidence/GR4/*.png` (17 downscaled frames, 405×828, from the
  `*-1` runs), verdict JSONs under `.cache/gr4/suite/*.json` (gitignored scratch).
- `EXPERIMENTS.md`: experiment `E13.8`; Backlog rows `G4a`, `G4b` (+ the R2a datapoint).

## 8. Reproduce

```bash
WT=/home/Qui/.paseo/worktrees/3oxa6xao/gr4
export CARGO_TARGET_DIR=$WT/.cache/gr4-target
cargo build --release --offline -p acos-cli -p acos-art
cd $WT
for b in newpos newneg; do
  timeout 1200 tools/art-runtime/run-exclusive.sh --task gr4 -- \
    tools/guarded.sh --mem 8G --time 570 -- bash .cache/gr4/run_suite.sh $b 3
done
timeout 1200 tools/art-runtime/run-exclusive.sh --task gr4 -- \
  tools/guarded.sh --mem 8G --time 400 -- bash .cache/gr4/run_suite.sh old
timeout 1200 tools/art-runtime/run-exclusive.sh --task gr4 -- \
  tools/guarded.sh --mem 8G --time 400 -- bash .cache/gr4/run_suite.sh oldneg
```

## 9. Host, limits, hygiene

Lock hold per batch 1.5–5.5 min including the queue wait (`run-exclusive.sh` starts its timer only
after `flock`); no batch exceeded the ~10 min rule. Host RAM peak observed 15/60 GB used, 40 GB in
cache, `available` never below 46 GB; load1 2.2–8.9. Disk `/` went 81 % → 83 % during the runs, so
the per-run frames/trees of the `-2`/`-3` repetitions and of the five merged negatives were pruned
afterwards (verdict JSONs kept for all 36 runs); `/` is back to 82 % with 167 GB free. Scratch stayed
under `$WT/.cache` (`gr4/` 4.4 GB, `gr4-target/` 1.1 GB); nothing under `/tmp`, no `sudo`, no
downloads, no `adb`, no phone use.
