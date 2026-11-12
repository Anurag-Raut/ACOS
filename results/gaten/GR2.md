# GR2 — Gaten past onboarding on real ART: the press *does* run; three ACOS walls behind it

Implementer/debugger task. Worktree `/home/Qui/.paseo/worktrees/3oxa6xao/gr2`, branch
`gaten-gr2-onboarding`, off `main` `5b8189b` (GR1 merged). Same APK as GR1, read-only:
`/home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk`
(sha256 `e04c7ffc…`). No Gaten source, bundle, asset or APK was modified; nothing in
`~/Projects/gaten` was touched, checked out, built or run.

**One-line verdict: GR1's first wall ("the onboarding press is delivered but the JS handler never
runs / `setOnboardingStep` fails") is disproved — the press runs, the `SharedPreferences` write
persists, and the whole owner flow (welcome → 3 Skip pages → Home → Analytics tab) executes on real
ART. `scenarios/gaten-home.yaml` passes 14/14 steps (rc=0). What it exposed instead are three
ACOS-side walls: a silent mis-target in `tap`'s selector resolution, an off-screen page reported as
`visible` in the guest tree (which is what made the Analytics step a *false pass*), and an
intermittent first-paint layout offset of exactly one display width.**

## 0. What GR1's wall actually was

GR1 §6/§7 left the onboarding press as the wall between "Gaten renders" and "the owner's flow
verifies", with `GatenAppInfo.setOnboardingStep` (a `SharedPreferences` write) as the first suspect.
I read what the press *does* in the app (read-only): `WelcomeScreen`'s "Set up Gaten" only starts a
Reanimated timing (`startWelcome` → `welcomeOut`); the first `setOnboardingStep` write happens on
page 1's **Skip** (`OnboardingFlow.write` → `onAdvance` → `GatenAppInfo.setOnboardingStep` →
`prefs().edit().putInt(KEY_ONBOARDING_STEP, step).apply()`).

Measured, all three at once:

1. **The press runs.** `tap "Set up Gaten"` → the next frame is onboarding page 1 (`1 / 3`, "Look up
   Japanese", "Turn on lookup", "Skip"), tree text changes from the welcome set to the page-1 set,
   `evidence/GR2/gaten-page1.png`. GR1's `gaten-art-02-after-setup.png` is the welcome screen's own
   looping highlight animation, not a transition — the screen never left the welcome timeline in
   that run.
2. **The write lands.** After one Skip the guest's own prefs file exists on the host:
   `<data>/runs/<id>/app-data/com.gaten/shared_prefs/gaten_settings.xml` →
   `<int name="onboarding_step" value="1" />`. So `apply()` (memory commit + enqueue disk write)
   works on this guest, and the promise resolves — `write`'s `await` returns and the page advances.
3. **The whole flow runs.** Welcome → page 1 (1/3) → page 2 (2/3, Anki) → page 3 (3/3, WaniKani)
   → Home (wordmark, search field, "Daily streak", Anki/WaniKani cards, "Recently looked up",
   Home/Analytics/Settings bar) → Analytics tab. Screens: `evidence/GR2/gaten-{welcome,page1,
   page2,page3,home}.png`.

`acos verify --engine art scenarios/gaten-home.yaml`: **rc=0, 14/14 steps pass**, 15.7 s wall,
`frames/frames_captured 14`, `artifact_validation: ok`. Full verdict:
`.cache/gr2/verify1/verdict.json` (not committed; the screenshots and the table below are).

So the GR1 hypothesis ledger's open item ("the JS press handler never runs … the settle is a
symptom") is **rejected**. What GR1 saw was a run in which *nothing about the tap was wrong except
the observation*: its tap was `NO_FRESH_FRAME` and its "after" capture was 2 s into the welcome
screen's loop, which is indistinguishable from an unchanged screen if you do not compare trees.

## 1. Walls hit, in order (hypothesis ledger)

| # | observation | hypothesis | experiment | result |
|---|---|---|---|---|
| W1 | GR1: tap handled, screen never advances, `setOnboardingStep` suspected | the native write throws or its promise never resolves | read `GatenAppInfoModule.kt` + `OnboardingFlow.write`; drive launch → tap → tree/screenshot; read the guest prefs file | **rejected**: the press advances to page 1 and `gaten_settings.xml` holds `onboarding_step=1`. The write is a plain `apply()`, no binder call |
| W2 | one run's second Skip tap hung with no reply for >7 min (run2), while a later run's identical step took 3 s | guest main thread wedged, or the launcher waiting on a settle bound | foreground `acos serve`, per-command `timeout`, `/proc` dump; repeat 4× | **unresolved, rare**: 1 of 6 launches. The wedge is *before* the tap reply, no guest crash marker, session still listed. Not the state write (W1 wrote fine in that same run); logged for the next task (§5 W-edge) |
| W3 | `tap {text: contains "Analytics"}` returns `pass`, "no visual change; no state change claimed", and the tab does **not** switch | the selector matched a node that is not the visible pressable | compare all `Analytics` nodes in `tree/11.json`; tap the tab's own bounds `(603,2233)` instead | **confirmed**: the tree carries a *second* `Home/Analytics/Settings` label row at `y=2151..2188` (x 203..532) next to the real bar (`content_desc` pressables at `[280,2151][453,2316]`, `[516,2151][690,2316]`); the text match's centre `(351,2169)` lands inside the **Home** segment. At `(603,2233)` the Analytics tab really selects (`evidence/GR2/gaten-analytics-broken.png` is the Analytics screen, tab bar showing Analytics selected) |
| W4 | the scenario's Analytics step passed although `frames/12.png` is still Home | `wait_for "Accuracy"` matched the off-screen, warm-mounted Analytics *page* | inspect the tree bounds of the Analytics scene | **confirmed**: the pager's next page is attached at `x=1080..2160` and every node in it reports `visible: true` (`Accuracy` at `[1138,1758][1309,1811]`), so `wait_for`/`assert_text` pass on content that is not on screen. The capture is the only evidence that contradicts it (§3) |
| W5 | `tap` steps pass while asserting nothing changed (`input_delivery … no visual change; no state change claimed`) | the verifier's `state_change` assertion is recorded, not enforced, for `tap` | `steps/11-tap.json` of the verify1 run | **confirmed by reading the artifact**: the assertion `passed: true` with the message "no visual change; no state change claimed". This is the mechanism that turns W3/W4 into a green scenario |
| W6 | 2 of 6 launches painted wrong: one welcome screen with the button reading "Set up" (the rest of the label missing) and highlighted glyphs missing (`なるほ` / `合` / `がいっ` with gaps), one launch painted **nothing at all** for 60 s | a text-fragment layout offset of exactly one display width (+1080 px), i.e. fragments are laid out as if they were on the pager's next page | `evidence/GR2/gaten-welcome-broken.png`, `gaten-white.png`; the same run's `wait_for "Set up Gaten"` returned `TIMEOUT … 65 polls` with a tree holding **zero** text nodes | **confirmed as the next real defect** (W6, §4): pixels and tree agree with each other, both are wrong; the app never recovers within the 60 s budget. Same family as GR1 §5.2(A) — the guest's layout/paint layer, not the app |

## 2. Evidence

`results/gaten/evidence/GR2/` (all PNGs downscaled 405×w from the 1080-wide C1 captures):

| file | what it is |
|---|---|
| `gaten-welcome.png` | the welcome screen from the passing `verify` run (frame 2) |
| `gaten-page1.png` | onboarding page 1 (`1 / 3`, lookup mode) — after "Set up Gaten" |
| `gaten-page2.png` | page 2 (`2 / 3`, AnkiDroid) |
| `gaten-page3.png` | page 3 (`3 / 3`, WaniKani token) |
| `gaten-home.png` | the Home tab: wordmark, search field, Daily streak (Mon…Sun), Anki / WaniKani cards, "Recently looked up", Home/Analytics/Settings bar |
| `gaten-analytics-capture.png` | the scenario's `gaten-analytics` capture — **the false pass**: same frame as `gaten-home.png` |
| `gaten-tap-analytics.png` | the frame the Analytics `tap` step bound to (`frames/11.png`), also Home |
| `gaten-analytics-broken.png` | the Analytics tab reached by its own bounds `(603,2233)` (run5): tab bar shows Analytics selected. The illustration/text damage is GR1 §5.2(A) at its worst (SVG drawn window-sized over the copy) |
| `gaten-welcome-broken.png` | W6: "Set up" instead of "Set up Gaten", highlighted glyph spans missing (they are laid out at +1080 px) |
| `gaten-white.png` | W6: a launch that never painted (60 s of `wait_for`, empty text tree) |

Host-side artifacts kept out of the repo (`.cache/gr2/`): `verify1/` (the passing verdict,
`steps/`, `tree/`, `frames/`, `logs/logcat.txt`), `run1…run5/` (per-command replies, prefs copies,
`/proc` dumps), `verify3/` (the W6 white-screen failure).

The passing run's logcat shows the app's own start and nothing between: `ReactNativeJS: Running
"Gaten"`, `ReactHost{0}.startSurface(surfaceId = 0): Execute`, `…ReactContext.onHostResume()` — no
JS exception, no `ReactNativeJS` error, no guest crash. The two `android.vending` /
`LauncherIconSystem` `IllegalArgumentException`s are the app's own optional-service probes (also on
the device) and are handled.

## 3. The false pass, precisely

The scenario is the definition of done, so this matters more than the new walls:

* step 11 `tap {text: contains "Analytics"}` binds the *only* frame evidence (`frames/11.png`), which
  is Home; the step's assertion is `input_delivery` with the message "no visual change; no state
  change claimed" and it is recorded as **passed** (W5);
* step 12 `wait_for "Accuracy"` passes in 13 ms off the off-screen pager page (W4);
* step 13 `capture gaten-analytics` therefore captures Home, and `artifact_validation` is happy.

Committed in this branch: the tap now names the tab bar's own pressable,
`selector: {content_desc: {exact: Analytics}}` (the one node that carries that description and is
the real `clickable`). With that selector the tap resolves to `(603,2233)`, which is the press that
provably selects the tab (W3). `assert_state` cannot be used as the proof yet: it has no `selected`
flag and `visible` is true for the off-screen page too (`assert_state … needs at least one expected
flag (visible/enabled/focused/clickable/scrollable/checked)`), so the *screen* evidence stays the
capture. **W4 and W5 are the fixes that would make the scenario prove Analytics by itself** (§5).

## 4. Next walls, in order (root cause named, fix sized)

1. **W6 — text/paint fragments laid out one display width off (intermittent, app-breaking).**
   A pressable label renders as "Set up" and the highlighted `<Text>` spans are missing because they
   are at `x+1080` (the pager's next-page offset) while their siblings are at `x`. The tree agrees
   (`+1080` bounds) and so do the pixels, so it is layout, not the probe. One launch painted
   nothing at all (`gaten-white.png`), which is the same failure one step earlier (the RN surface's
   first commit never reached the compositor). It breaks `wait_for`/`tap` by text and it is what
   makes a green run luck-dependent. Root layer: the guest's shadow-tree/fragment layout
   (`Yoga` fragment offsets through the ACOS `ViewRootImpl`/`Canvas` path) and the first-frame
   commit — the same layer as GR1 §5.2(A) (`canvas.getWidth()` is the *window's* for every view).
   Fix size: large (per-view RenderNode / HWUI path), so this is the stop-and-report point.
2. **W4 — the guest tree calls off-screen pager pages `visible`.** `visible` is currently
   `isShown()`-like (attached + own rect), not "intersects the display and is not clipped away by an
   ancestor scroll/pager". Root layer: the in-guest tree probe (`LaunchMain`'s `ACOSLAUNCH_LIVE_PROBE`
   sample). Two candidate fixes, both small: report `visible:false` for a node whose (clipped) bounds
   do not intersect the display, and/or add the clipped rect to the reply so selectors and
   `wait_for` can require on-screen. This removes the false-pass class, not just this instance.
3. **W5 — a `tap` may pass while nothing changed.** Either the scenario `tap` must be able to demand
   `state_change`, or the verifier should fail a tap step whose `state_change` assertion reports
   "no visual change; no state change claimed" while both channels are available. Root layer:
   `acos-verify` (scenario semantics). Not changed here: it is a product-wide semantic decision
   (it would turn `scenarios/gaten-home.yaml` red until W4 is fixed), and the gates for it are not
   in this task's list.
4. **W3 — duplicate/phantom label row overlapping the real tab bar.** The tree carries two
   `Home/Analytics/Settings` rows; the first (text-only, `y 2151..2188`) belongs to a hidden
   copy of the bar (the material-top-tabs bar the app draws its own bar over) and its centre falls
   inside the *neighbouring* segment. Until the probe can be asked for "the pressable", text
   selectors on this screen are ambiguous; `content_desc` is the workaround, `index`/`bounds` are
   the escape hatch. Same fix as W4 (clip/visibility-aware probe) makes the phantom row not a
   candidate.
5. **W-edge — one wedge in six runs** (W2). The second Skip's tap never returned a reply for
   >7 min; the daemon was killed by the harness timeout. Needs the guest-side stuck dump
   (`SessionMain` has one; the `LaunchMain` interactive path does not) and a `timeout`-bounded
   launcher. Not diagnosed in this budget; no guest crash marker was written.
6. GR1's own items remain open and are *not* re-opened here: per-view canvas/HWUI (§5.2 A),
   carved-vs-inset window model (§5.2 B), settle on continuously animating screens, guest teardown.

## 5. Limits of this run

* Six launches total. One (run2) wedged mid-flow (W2), one (verify3) painted nothing (W6), one
  (run4) painted the +1080-labelled welcome (W6/W5), three were clean (run1, run3, verify1). The
  green verdict is real for the run it describes; it is not yet a *stable* pass, and W6 is the
  reason.
* The Analytics screen was reached and captured once (run5), by tapping the tab bar's own bounds.
  Its rendering is heavily damaged (GR1 §5.2 A: SVG/canvas drawn window-sized over the copy), so no
  visual comparison of Analytics against the device was attempted.
* `acos verify` ran with the scenario's own display (`1080x2400 @ 400 dpi`); the interactive runs
  used `--reference`, which is the same configuration.
* No ACOS source was changed for the functional wall — the press, the write and the flow needed no
  fix; the only committed product-side change is the scenario's tap selector (§3). W4/W5/W6 are
  diagnosed and sized, not fixed.

## 6. Commands run

```bash
# build the two binaries the daemon spawns (the ART engine needs $ACOS_ART_BIN)
cargo build --release --offline -p acos-cli -p acos-art

export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_ART_BIN=<worktree>/target/release/acos-art
export ACOS_RUNTIME_DIR=/home/Qui/Projects/acos/.cache/gr2/rt
export ACOS_SESSION_SOCKET=/home/Qui/Projects/acos/.cache/gr2/rt/s.sock

# every ART run (lock held ≤ 10 min per acquisition, scratch under .cache/gr2)
tools/art-runtime/run-exclusive.sh --task GR2 -- \
  tools/guarded.sh --mem 8G --time 480 -- bash /home/Qui/Projects/acos/.cache/gr2/runN.sh

# the owner flow, as the definition of done
tools/art-runtime/run-exclusive.sh --task GR2 -- tools/guarded.sh --mem 8G --time 440 -- \
  target/release/acos verify scenarios/gaten-home.yaml --engine art \
  --artifacts /home/Qui/Projects/acos/.cache/gr2/verify1 --json

# the interactive diagnosis loop (inside those scripts)
acos launch <apk> --engine art --keep --reference --json
acos observe --session s_cli --tree --json
acos observe --session s_cli --screenshot out.png --json
acos tap --session s_cli --text "Set up Gaten" --json
acos tap --session s_cli --text Skip --json          # x3
acos tap --session s_cli 603 2233 --json             # the Analytics segment's own centre
acos logs --session s_cli --json
acos session destroy --session s_cli --json
```

Prefs proof (host side, from the run's own data dir):

```
$ cat …/runs/<id>/app-data/com.gaten/shared_prefs/gaten_settings.xml
<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <int name="onboarding_step" value="1" />
</map>
```

## 7. Validation

| check | result |
|---|---|
| `cargo test --release --offline -p acos-cli -p acos-art -p acos-services` | **rc=0**, 65 test binaries, **552 passed / 0 failed** (`out/tests`, log in `.cache/gr2/tests.log`) |
| `acos verify --engine art scenarios/gaten-home.yaml` (before the selector change) | **rc=0**, 14/14 steps, `artifact_validation ok` (`.cache/gr2/verify1/verdict.json`) |
| the seven mandatory gates | **not re-run**: this branch changes no product code (`git diff 5b8189b -- crates/ tools/` is empty) — two documents, one evidence directory and one scenario step's selector. Gate baselines are unaffected by a scenario file no gate loads. The earlier gates on `main` (`results/gaten/GR1-gates.txt`) stand. |
| the updated scenario (selector change) | queued behind the host ART lock for this whole session (≥ 6 concurrent agents; the queue never drained inside the budget). The change is *reasoned, not yet re-verified as a whole run*: the selector it now uses (`content_desc {exact: Analytics}`) is the node at `[516,2151][690,2316]`, and a tap at that node's centre was proven to select the tab (run5, `gaten-analytics-broken.png`). **A clean run of the scenario is the first thing to do next** — and it can still fail on W6 (the +1080 px layout flake), which is exactly why the fix order in §4 is W6 first. |

An honest note on the green verdict: it is a real pass *for the run it describes* (14 steps, 14
frames, a frame per step, the guest's own exit record `outcome: ok`, `guest_exit: 0`,
`oom_kills: 0`), and it is what the owner asked to see — Gaten past onboarding on real ART. It is
not yet a *stable* pass, because 2 of 6 launches painted wrong (§4 W6), and it does not yet *prove*
the Analytics screen, because the tree reports off-screen pages as visible (§3, §4 W4/W5).
