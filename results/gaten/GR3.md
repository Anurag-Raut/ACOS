# GR3 — Gaten flows on real ART: the screens past Home, the animating-screen wall, and the probe wake

Implementer task. Worktree `/home/Qui/.paseo/worktrees/3oxa6xao/gr3`, branch
`gaten-gr3-flows`, off `main` `10778f9`. APK read-only:
`/home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk`
(sha256 `e04c7ffc…`). No Gaten source, bundle, asset or APK was modified; nothing in
`~/Projects/gaten` was touched, checked out, built or run. No secrets were read.

**One-line verdict: the owner's app now verifies past Home on real ART for the tabs, Settings
(including a real theme change), the dictionary lookup/word page, and the Anki/WaniKani lesson
gate — 3/3 each except the tab run that lost one rep to the known SIGSEGV flake (5/6). The welcome
and onboarding pages animate forever (Reanimated 10 s loop + a 200 ms auto-advance retry), so they
are honestly not strict-settled; their taps are labelled `settle: fast` navigation and every claim
after them is proven strictly. The probe wake was over-driven by the host; it is throttled in ACOS.**

## 0. What changed in ACOS

| file | change | why |
|---|---|---|
| `crates/acos-protocol/src/scenario.rs`, `schema/scenario.json` | input steps (`tap`/`type`/`key`) accept `settle: strict|fast` | the API already had fast input (`settle:"fast"` on `acos_tap`/`acos_text`/`acos_key`); scenarios could not ask for it. Assertions still reject fast. |
| `crates/acos-verify/src/executor.rs`, `backend.rs` | pass the step's settle choice to the backend | same |
| `crates/acos-cli/src/engine/art.rs`, `mod.rs`, `trust.rs` | forward the input settle mode to `ArtSession::settle_mode` through `TrustGuard` | otherwise the scenario said fast and the guest still ran strict |
| `crates/acos-art/src/launch.rs` | strict-settle probe wake: 50 ms cadence (was 20 ms) and only when no newer probe sample has arrived since the previous request; `ACOS_STRICT_SETTLE_WAKE_MS` overrides | the host asked the guest for a full tree walk 50×/s while Gaten was already emitting dirty samples; this is the likely cause of the `probe-floor` starvation |

No settle rule, selector visibility, capture binding or assertion was weakened. `wait_for` and every
`assert_*` remain strict. A screen that never settles still times out honestly (§4).

## 1. Screen map (from `.cache/gaten/src-dev`, verified in the trees/pixels)

| area | screens / states | route | settles strictly? |
|---|---|---|---|
| onboarding | Welcome; pages 1/3 lookup, 2/3 Anki, 3/3 WaniKani; Skip on each | `OnboardingFlow` | **no** — Welcome runs `clock = withRepeat(withTiming(10, 10 s))`; each page arms a 200 ms `AUTO_ADVANCE_RETRY_MS` timer |
| Home | wordmark, lookup-status button, search field, Daily streak, Anki/WaniKani service cards, Recently looked up, bottom bar | `HomeTab` | yes (after onboarding) |
| Analytics | Anki/Collection, WaniKani/Progress, Study activity, Accuracy, Level progress (below the fold) | Analytics tab | yes |
| Settings hub | Lookup (Screen lookup, Text recognition), Study (Anki cards, WaniKani, Kanji reference), App (Appearance, Support Gaten, About) | Settings tab | yes |
| Settings pages | Lookup, Text recognition, Anki cards, WaniKani, Kanji reference, Appearance (System/Light/Dark + 7 accents + Haptics/Sounds), About (privacy, attributions, licence) | `SettingsPage` | yes |
| Search | idle prompt + example chips, query/results list, “N results”, prefix group, result card | Home's own stack | yes (results settle after the debounce) |
| Word details | headword/reading/POS, Meanings, Other spellings, kanji strip (when a reference is connected), Example sentences, Add to Anki | Home stack `Word` | yes |
| Review (Anki) | opening → blocked (“Anki is not installed”, Download Anki) → decks/card/rating when AnkiDroid exists | Home stack `Review` | blocked page yes |
| WaniKani | connect form (token field, Create a token, Connect WaniKani) → hub (Reviews/Lessons) → review/lesson quiz | Home stack `Review` | connect page yes; queue needs a token |
| dialogs/modals | Anki setup help, leave-session confirm, support sheet, download confirm | over the current page | not exercised (needs a session/help path) |

**Not reachable on this guest (data, not ACOS):** Anki deck list/card/rating and WaniKani
lesson/review/quiz. AnkiDroid (`com.ichi2.anki`) is not installed, and no WaniKani token is stored;
the real lesson flow needs one of those accounts. The scenario proves the gate, the blocked state,
the connection form and text input instead of fabricating a lesson.

## 2. Scenarios (all run under the ART lock, one scenario per invocation)

Common opening: launch `com.gaten.app.MainActivity` `clear_state`, one strict tap on “Set up
Gaten” (`settle_timeout_ms: 5000` — this waits through the welcome timeline to page 1), three
`settle: fast` Skip taps, then a strict Home wait. Every `assert_text` runs on a capture's bound
frame; the fast taps are navigation only.

| scenario | steps | runs | result | wall p50 | evidence |
|---|---|---:|---|---:|---|
| `scenarios/gaten-home.yaml` | 14 | 3 | **3/3 pass** | 5.90 s | `evidence/GR3/gaten-tabs-home.png` (same Home) |
| `scenarios/gaten-tabs.yaml` | 22 | 6 | **5/6 pass**; one `GUEST_EXITED` SIGSEGV at launch (known R2a) | 9.00 s | `gaten-tabs-{home,analytics,settings,home-again}.png` |
| `scenarios/gaten-settings.yaml` | 33 | 3 | **3/3 pass** | 11.2 s | `gaten-settings-{home,hub,appearance,appearance-dark,hub-dark,about,back-home}.png` |
| `scenarios/gaten-search.yaml` | 22 | 3 | **3/3 pass** | 8.9 s | `gaten-search-{home,results,word,results-again}.png` |
| `scenarios/gaten-lesson.yaml` | 28 | 3 | **3/3 pass** | 10.1 s | `gaten-lesson-{home,anki-blocked,home-again,wanikani-connect,token-entered,back-home}.png` |

The tab run that failed did not reach a step assertion: launch returned `GUEST_EXITED`, signal 11,
`SIGSEGV (SEGV_ACCERR)` in thread 2 (`com.gaten`) with the R3 evidence path
`/apex/com.android.runtime/lib64/bionic/libc.so`; see `.cache/gr3/runs/tabsb-2/art-frames/`.
It is the same R2a/R3 launch flake, not a scenario or selector failure. The 3/3 tab rerun (`tabsc`)
after it passed with the identical scenario.

### 2.1 Step table (representative pass, `tabsb-1`)

| # | step | target | result | ms |
|---:|---|---|---|---:|
| 1 | launch | MainActivity, clear state | pass | 1548 |
| 2 | tap strict | “Set up Gaten” | pass | 1300 |
| 3–5 | tap fast | Skip ×3 | pass | 94 / 74 / 74 |
| 6 | wait_for | “Recently looked up” | pass | 292 |
| 7 | capture | `tabs-home` | pass | 87 |
| 8 | assert_text | “Daily streak” | pass | 12 |
| 9 | tap | `content_desc Analytics` | pass | 1334 |
| 10 | wait_for | “Study activity” | pass | 104 |
| 11 | capture | `tabs-analytics` | pass | 104 |
| 12–13 | assert_text | “Accuracy”, “Anki is not installed.” | pass | 12 / 13 |
| 14 | tap | `content_desc Settings` | pass | 1293 |
| 15 | wait_for | “Screen lookup” | pass | 89 |
| 16 | capture | `tabs-settings` | pass | 90 |
| 17–18 | assert_text | “Screen lookup”, “Kanji reference” | pass | 12 / 12 |
| 19 | tap | `content_desc Home` | pass | 1304 |
| 20 | wait_for | “Recently looked up” | pass | 91 |
| 21 | capture | `tabs-home-again` | pass | 89 |
| 22 | assert_text | “Daily streak” | pass | 12 |

Full per-step JSON/PNG/tree: `.cache/gr3/runs/{home2,tabsb,tabsc,settingsb,searchb,lessonb}-*/`
(gitignored). Negative controls: `scenarios/negative/gaten-*-wrong.yaml`, each **fail /
ASSERTION_FAILED** on one run (0 false passes):

| negative | verdict | code | steps |
|---|---|---|---|
| `gaten-home-wrong` | fail | ASSERTION_FAILED | 8 |
| `gaten-tabs-wrong` | fail | ASSERTION_FAILED | 8 |
| `gaten-settings-wrong` | fail | ASSERTION_FAILED | 13 |
| `gaten-search-wrong` | fail | ASSERTION_FAILED | 17 |
| `gaten-lesson-wrong` | fail | ASSERTION_FAILED | 11 |

## 3. Timing, animations off vs on (gaten-tabs, same host, ART lock, load recorded)

Off: 5 passing runs (`tabsb` 1/3 + `tabsc` 1–3; `tabsb-2` is the SIGSEGV). On: 3 passing runs
(`tabson`). Host load1 was 2.2–8.9 (off) and 6.5–7.9 (on); RAM used 13.1–15.3 GB of 61.9 GB
(21–25 %, under the 90 % cap).

| metric | animations off | animations on |
|---|---:|---:|
| wall p50 | **8.997 s** (8.882–9.323) | **10.356 s** (9.629–10.365) |
| launch p50 | 1548 ms | 1561 ms |
| tap p50 (28 taps/run, mixed strict/fast) | **1307 ms** | **1339 ms** |
| wait_for p50 | 103 ms | 118 ms |
| capture p50 | 94 ms | 113 ms |
| assert_text p50 | 12 ms | 15 ms |

The functional flow is dominated by the three tab presses (`tap` p50 ~1.3 s each: the framework's
own press/elevation animator is not what the `animations:off` switch removes on this RN/Fabric
screen; the PostTap settle deadline is), and by the 1.5 s strict setup tap. `animations:off` saves
~1.36 s per tab scenario here, not the counter fixture's 7–8× tap win.

## 4. The animating-screen wall and the probe wake (orchestrator priority)

### 4.1 What keeps Gaten drawing (source answer, no phone)

* `src/rn/ui/onboarding/WelcomeScreen.tsx:249` — a Reanimated shared value runs
  `clock.value = withRepeat(withTiming(10, {duration: LOOP_MS, easing: linear}), -1, false)`.
  The timeline is 10 s and repeats forever, so the welcome screen stores a new C1 frame on every
  animation frame. Reanimated drives its own frames; Android's three animator scales (what
  `animations: off` sets) do not stop it. On a real phone this is the intended welcome animation.
* `src/rn/ui/onboarding/OnboardingFlow.tsx:156,1142,1157` — `AUTO_ADVANCE_BEAT_MS = 600` and
  `AUTO_ADVANCE_RETRY_MS = 200`. While a page's gate is not verified, `fireAutoAdvance` re-arms a
  200 ms `setTimeout`; 200 ms is inside the strict settle horizon (256 ms), so the guest's own
  event rule reports `queue` forever. On a real phone the retry also runs; it is invisible because
  the page looks static.
* `OnboardingArtwork`/`StepList` use Reanimated springs/timings for entrance and step changes;
  those settle, the two loops above do not.

Therefore the welcome/onboarding screens **do not settle**, by construction. Home, Analytics,
Settings, the search list, the word page, the Anki blocked page and the WaniKani connect page do.

### 4.2 Probe starvation (`probe-floor`, ~12 s tree age)

`wait_for_event_settled` requested a guest probe every 20 ms for the whole settle deadline
(`request_guest_probe`). The guest coalesces requests, but a request deliberately bypasses the
probe thread's 25 ms min-gap, so under a 60 fps Reanimated loop the host was asking for ~50 full
view-tree walks/s while the app was also marking the tree dirty on every draw. The main thread is
shared by the probe walk, the choreographer callbacks and the RN/Reanimated frame; when it falls
behind, the probe sample stops advancing and the host sees only the last sample (`probe-floor`,
`tree_age_ms` ~12 s, `probe_seq` frozen).

Fix in this branch: the host wakes only when the newest probe sequence has not advanced since the
previous request, at 50 ms instead of 20 ms (`ACOS_STRICT_SETTLE_WAKE_MS`). A drawing screen uses
its own dirty samples; an idle screen still gets a bounded wake. This removes the host-side flood
but does **not** make a permanently animating screen settle: it still times out honestly. It also
does not prove the guest main thread can never saturate under a pathological animation; the wall is
now named and instrumented (`ACOSPROBE-STALL-DETECT`, `ACOSPROBE-STALL main-thread`).

### 4.3 Verification-semantics options (proposal only — owner decision)

Until one is chosen, `wait_for`/`capture`/`assert_*` on the welcome/onboarding pages honestly fail
`TIMEOUT`. The options, with the false-pass each buys:

1. **Stable tree content, ignore frame generation.** Accept when the tree's content signature is
   unchanged for N ms even though C1 keeps storing. *False pass:* a purely visual animation (SVG
   drift, opacity, canvas) has a constant tree; an assertion could read a tree from an older draw
   than the pixels it is bound to, and a transient layout that happens to repeat could look stable.
2. **Region mask / stable region.** Settle on a caller-specified display region (e.g. the form, not
   the artwork). *False pass:* a change outside the mask can still be the state under test; a wrong
   mask silently hides it. Needs per-step regions and host pixel proof, and still cannot prove a
   full-screen transition.
3. **Navigation-only fast, assertions elsewhere** (what these scenarios do explicitly): `tap/type/
   key` may use the existing fast rule; `wait_for`/`capture`/`assert_*` stay strict. *False pass:*
   none added, but an action on an animating screen is only delivery + a tree-change claim; the
   scenario must prove the new screen strictly afterwards (it does, on Home and every later step).
   A `wait_for` that only wants presence could be given an explicit `settle: fast` navigation label
   with the same rule: a pass there is never assertion evidence.
4. **Explicit non-settle assertion.** Add an assertion mode that reads an unsettled tree and says so
   in the verdict. *False pass:* exactly what the mode allows; only acceptable if the verdict is
   labelled non-evidence and cannot satisfy a `pass` expectation. Not recommended.

Recommendation: keep (3) as the only implemented behaviour and let the owner decide whether (1) or
(2) should exist for full-screen animation; do not weaken strict capture/assert.

## 5. Walls fixed vs classified

| ID | observation | lowest layer | status |
|---|---|---|---|
| G3a | Scenario input could not request the API's existing `fast` navigation settle, so every tap on the 200 ms-retry onboarding pages was a strict `queue` timeout | verify scenario schema/executor + backend forwarding (`trust.rs` too) | **fixed** (this branch; 3/3 tabs/settings/search/lesson after it) |
| G3b | Host strict-settle probe wake was 20 ms and re-asked even after a sample arrived; ~50 tree walks/s under Gaten's 10 s loop is the likely `probe-floor` cause | `acos-art` host strict-settle loop | **fixed in code** (50 ms + no re-ask without a new sample); not yet proven at a 60 s pathological n |
| G3c | Launch can return a 1-node blank tree as `first_frame` (the old W6/R2a first-paint shape). An assert on the launch frame then fails even though the app paints a moment later | ART launcher first-frame predicate / Gaten first paint | **classified, mitigated in scenarios**: no assert on the launch frame; the first strict tap re-traverses and waits through the paint (home 3/3). Root cause still open under R2a/R2b |
| G3d | `gaten-tabs` lost 1 of 6 runs to a post-first-frame SIGSEGV (`SEGV_ACCERR`, thread 2 `com.gaten`, libc frame). R3 evidence names signal/address/thread/library | ART guest native fault | **classified** as R2a (known, 1/10-ish); no app or scenario fault, no false pass |
| G3e | No verification semantics for a screen that animates forever in a region that does not matter | verify semantics | **owner decision** (§4.3); not implemented |

## 6. Gates and tests

All under the host ART lock (`run-exclusive.sh --task GR3`), one scenario per invocation:

| gate | result |
|---|---|
| `scenarios/v2-art-counter.yaml` | pass (11 steps) |
| `scenarios/w22-counter-reset.yaml` | pass (9) |
| `scenarios/w54-dialog-pixels.yaml` | pass (14) |
| `scenarios/w42-back-frame.yaml` | pass (17) |
| `scenarios/w15-dialog-menu.yaml` | pass (3) |
| `scenarios/form-validation.yaml` | pass (28) |
| `scenarios/suite/T03-render-good.yaml` | pass (5) |
| `scenarios/v1-offscreen-pager.yaml` | pass (9) |
| `scenarios/suite/T03-render-wrong.yaml` | **fail / ASSERTION_FAILED** (expected control) |

`cargo test --release --offline`:

* `-p acos-protocol` **green** after `UPDATE_SCHEMA=1` (schema `scenario.json` refreshed);
* `-p acos-verify` **green** (19 integration tests + unit);
* `-p acos-cli` **green with one known pre-existing flake**: the first full run failed
  `serve_daemon::daemon_holds_a_native_session_across_cli_invocations_and_idles_out`
  (`/tmp/.../session.sock must exist after --keep`); it passes standalone (3.29 s) and the full
  crate passes with that test skipped (all binaries green). This is the same serve-daemon flake
  recorded in E13.4, unrelated to the changed files;
* `-p acos-art` **green** (all 14 binaries).

## 7. Files

* `scenarios/gaten-home.yaml` (rewritten opening: no assert on the launch frame; 3 Skip taps are
  fast navigation, 14 steps, 3/3).
* `scenarios/gaten-tabs.yaml`, `gaten-settings.yaml`, `gaten-search.yaml`, `gaten-lesson.yaml`.
* `scenarios/negative/gaten-{home,tabs,settings,search,lesson}-wrong.yaml` (expectation fail,
  all measured fail/ASSERTION_FAILED).
* `results/gaten/evidence/GR3/*.png` (21 downscaled 405-wide captures).
* ACOS: `crates/acos-protocol` (scenario `settle` + schema + test constructors),
  `crates/acos-verify` (executor/backend), `crates/acos-cli` (engine forwarding, trust),
  `crates/acos-art` (strict probe wake pacing).
