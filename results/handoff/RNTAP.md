# RNTAP — RN tap visibility: why `acos_tap {"text": "<tab label>"}` refused, and the rule that fixes it

**Worktree:** `/home/Qui/.paseo/worktrees/3oxa6xao/s3ab`
**Branch:** `rn-tap-visibility` (from `rel3-int` @ `19db6b8` = v0.2.0 release candidate).
Not pushed, not merged. Track: RNTAP.

---

## 1. Root cause (two facts, one bug)

**Fact 1 — the `visible`/`clickable` values are the framework's real answer, not an
ACOS mapping bug.** The Gaten bottom tab bar as the API-34 guest reports it
(`.cache/rntap/tabs-home.json`, 345 nodes, captured after the three onboarding
Skips):

| ACOS node path (tail) | class | text | content_desc | bounds | visible_bounds | visible | clickable |
|---|---|---|---|---|---|---|---|
| `…1.1.0.0.0` | `X4.e` | `Home` | — | [203,2151][286,2188] | [203,2151][286,2188] | **false** | false |
| `…1.1.0.0.1` | `X4.e` | `Analytics` | — | [286,2151][416,2188] | [286,2151][416,2188] | **false** | false |
| `…1.1.0.0.2` | `X4.e` | `Settings` | — | [416,2151][532,2188] | [416,2151][532,2188] | **false** | false |
| `…1.1.0.1.0` | `b5.g` | — | `Home` | [280,2151][453,2316] | [280,2151][453,2316] | true | **true** |
| `…1.1.0.2.0` | `b5.g` | — | `Analytics` | [516,2151][690,2316] | [515,2151][690,2316] | true | **true** |
| `…1.1.0.3.0` | `b5.g` | — | `Settings` | [689,2151][862,2316] | [688,2151][862,2316] | true | **true** |
| `…1.1.0.5.0.0.1` | `X4.e` | `Home` | — | [362,2214][445,2251] | [361,2214][445,2251] | **true** | false |
| `…1.1.0.5.0.1.3` | `X4.e` | `Analytics` | — | [292,2214][422,2251] | [290,2214][422,2251] | **false** | false |
| `…1.1.0.5.0.2.1` | `X4.e` | `Settings` | — | [251,2214][367,2251] | [250,2214][367,2251] | **false** | false |
| `…0.2.0` (Analytics page content) | `b5.g` | — | `Analytics, Refresh the numbers, …` | [1138,128][2102,2628] | **—** | **false** | true |

Three things this table says:

1. The `visible: false` label rows **have on-screen `visible_bounds`**, so the answer is
   not clipping: `View.isVisibleToUser(Rect)` returned false through the alpha/visibility
   branch (`base/core/java/android/view/View.java:11455-11513`: `mWindowVisibility`,
   then every ancestor's `getAlpha()/getTransitionAlpha()/getVisibility()`), not through
   `getGlobalVisibleRect`. React Native's tab bar lays its label row out and keeps the
   inactive labels at alpha 0 — the bar draws a label for the **active** tab only.
2. **A real device agrees.** The moto g35 screenshot of the same screen
   (`results/phone/gaten-f344272/gaten-tabs/phone/screenshot-06-capture-tabs-home.png`,
   tab bar cropped at `.cache/rntap/phone-tabbar.png`) shows one pill with the word
   “Home” and **two icon-only tabs**. Nothing in ACOS's tree claims less than the device
   shows. `visible` is copied verbatim from the guest's `isVisibleToUser()`; TREE1 that
   introduced the `AccessibilityNodeInfo` mapping did not touch that computation
   (`git log -S'visibleToUser(v)'` — unchanged by TREE1).
3. The tab you can actually press is a **text-less `ViewGroup` whose whole name is its
   `contentDescription`** (`Analytics`, `Settings`, `Home`) and it reports
   `visible: true, clickable: true` with correct bounds. RN says so; so does a device.

**Fact 2 — the bug is in *tap target resolution*, and it was pre-existing.** `acos_tap
{"text": "Settings"}` built `Selector::by_text` and resolved through `find_one`
(`crates/acos-protocol/src/ui.rs`), which matches `node.text`/`node.hint` only. For Gaten
that matches *only* the two invisible label `TextView`s, so `tap_text`
(`crates/acos-cli/src/engine/art.rs`) fell into `find_invisible` and answered
`ELEMENT_OFFSCREEN` — honest, but useless: the name the agent read on screen lives on
the pressable, not on any text node. The five Gaten scenarios never hit it because they
target `content_desc` (`scenarios/gaten-tabs.yaml:41,50,60`: “the tab pressable's own
content_desc, never the duplicated label”), which is the selector field that already
reaches the pressable. That is the whole story of “the scenarios pass but an agent's
`acos_tap {"text": …}` does not”.

Why the platform's rule is the accessible name: `AccessibilityNodeInfo` is the object
UiAutomator and an accessibility service read, and its **name** is the node's text, else
its content description (`AccessibilityNodeInfo.getText()`, else
`getContentDescription()`; UiAutomator exposes the two as `By.text`/`By.desc`, and
`agent-device`'s `label` is the same computed name — `results/handoff/TREE1.md` §4). A
React Native `Pressable` never sets text, so on any device the reachable name is the
content description. ACOS's *evidence* selectors stay strict (`assert_text` must not
pass on a description), but *input* resolution has to use the name the platform uses.

## 2. What changed

### 2.1 `crates/acos-protocol/src/ui.rs` — `UiTree::find_input_target`

New, documented, and additive:

1. the ordinary field match among user-visible nodes (`find_one`, unchanged);
2. only when that finds nothing **and** the selector is a `text` selector: a
   user-visible node whose own `text` is empty and whose `content_desc` matches (the
   remaining selector fields still have to match; `index` still applies).

Only user-visible nodes are returned, so the off-screen gate is untouched: an invisible
node still resolves to `None` and the caller still answers `ELEMENT_OFFSCREEN`.

### 2.2 `crates/acos-cli/src/engine/art.rs` — `tap_text` uses it

The tap path now calls `find_input_target`. That covers every tap-by-text route:
MCP `acos_tap {"text"}`, CLI `acos tap --text`, and a scenario `tap: {selector: {text}}`
step (the executor's `InputAction::Tap` → `tap_node` → `tap_text`). `find_one`/`find_all`
and every assertion keep the strict, field-exact semantics.

### 2.3 Docs

`docs/AGENT_GUIDE.md`: a new paragraph in the selectors section (“Visited vs. named: what
a `tap` selector resolves to”) that states the `visible` = `View.isVisibleToUser()`
contract, the accessible-name rule for taps, the `ELEMENT_OFFSCREEN` guarantee, and the
RN `accessibilityLabel` case; plus the workflow step 5 wording. `crates/acos-mcp/tests/
guide.rs` stays green.

### 2.4 `tools/release/mcp_smoke.py`

* `gaten` mode now **taps by text**: it walks the onboarding by text, then reads the
  bottom-tab label out of the tree and calls `acos_tap {"text": label}` — no bounds
  fallback any more (the old comment “React Native reports clickable/visible false … so
  the tap uses the node's bounds centre” is gone). It then requires `state_changed:
  true` and the tab's own screen text (`Analytics` → `Study activity`, `Settings` →
  `Screen lookup`, `Home` → `Recently looked up`) within a bounded 20 s poll.
* `acos_help` check updated to REL3b F12's real contract: no topic returns the overview
  *skeleton* (>1500 chars), `{"topic": "all"}` returns the whole guide (>20000 chars and
  contains `acos_tap`). The old `>5000` threshold was written before the skeleton existed
  and would fail every correct server.

## 3. Acceptance evidence

Every ART run and every test went through
`/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task RNTAP -- tools/guarded.sh …`.

### A. MCP, installed scratch prefix, outside the checkout, default settings — **PASS**

```
tools/release/install.sh --from-target WT/.cache/s3ab-target \
    --prefix WT/.cache/rntap/prefix          # ACOS 0.2.0
```

Run from `/home/Qui/Projects/acos-rntap-outside` (holds only `mcp_smoke.py`) with `env -i`
(HOME, USER, LANG, PATH, XDG_RUNTIME_DIR, DBUS_SESSION_BUS_ADDRESS,
`ACOS_ALLOWED_ROOTS=/home/Qui`, `ACOS_ARTIFACTS_DIR=<outside>/artifacts`). **No
`ACOS_SESSION_DAEMON`, no `ACOS_BIN`, no engine argument.** Transcript:
`.cache/rntap/mcp-gaten.jsonl` (final line `{"smoke": "ok"}`).

| check | result |
|---|---|
| `acos_launch` | pass, `fidelity art-reference`, `engine art`, ladder `process/activity_resumed/first_frame` all ok |
| `observe_tree gaten` | 36 real Gaten texts |
| `gaten_onboarding_tap_by_text` ×3 | `Set up Gaten`, `Skip`, `Skip` — all `accepted: true` **by text** |
| `acos_tap {"text": "Analytics"}` | `status pass`, `accepted true`, **`state_changed true`** (`settle.bound strict-event`, `first_change_ms 180`) |
| `gaten_tab_screen_text` | after the tap the tree carries `Analytics` **and `Study activity`** (the Analytics screen) and differs from the Home tree |
| `acos_destroy` / `acos_status` | destroyed + teardown verified, then `SESSION_NOT_FOUND` |

### B. 15 gates — **15/15 as specified**

| scenario | expected | observed |
|---|---|---|
| `v2-art-counter`, `w22-counter-reset`, `w54-dialog-pixels`, `w42-back-frame`, `w15-dialog-menu`, `form-validation`, `suite/T03-render-good`, `v1-offscreen-pager`, `w7-insets-report`, `se1-expect-change-tap` | pass | **pass** |
| `suite/T03-render-wrong`, `w7-insets-carve-negative`, `negative/se1-expect-change-noop` | fail `ASSERTION_FAILED` | **fail `ASSERTION_FAILED`** |
| `negative/offscreen-pager-visible` | fail `ELEMENT_OFFSCREEN` | **fail `ELEMENT_OFFSCREEN`** |
| `p11-progress-timeout` | timeout `TIMEOUT` | **timeout `TIMEOUT`** |

### C. Gaten flows and negatives — **10/10 and 9/9**

`gaten-home`, `gaten-tabs`, `gaten-settings`, `gaten-search`, `gaten-lesson` (5 owner
flows) and `gaten-support`, `gaten-kanji`, `gaten-recognition`, `gaten-backstack` (GR4):
**all pass**. `negative/gaten-{home,tabs,settings,search,lesson,support,kanji,recognition,
backstack}-wrong`: **all fail `ASSERTION_FAILED`** at their own mutated step.

Raw logs + per-run JSON: `.cache/rntap/sweep/summary.txt`, `.cache/rntap/sweep/*.json`,
artifacts under `.cache/rntap/sweep/artifacts/<scenario>/`.

### D. Tests — **PASS (525 passed, 0 failed)**

`cargo test --release --offline -p acos-cli -p acos-art -p acos-mcp -p acos-verify
--no-fail-fast -- --test-threads=1` (46 test targets, log `.cache/rntap/cargo-test2.log`):
525 passed, 0 failed, including `acos-mcp/tests/guide.rs` (the guide asserts its own
sections) and the `acos-art` `launch_hang` latency bound.

The new unit tests are `crates/acos-cli/src/engine/mod.rs`:

| test | asserts |
|---|---|
| `rn_tab_labels_keep_the_frameworks_visibility_answer` | the guest's `visible` is copied verbatim: the undrawn label is `false` **with** `visible_bounds`, the pressable is `true`/`clickable` |
| `tap_by_text_resolves_the_accessible_name_not_a_decorative_label` | `find_one("Settings")` is `None`, `find_input_target("Settings")` is the pressable, tap point `(775,2233)` |
| `a_real_text_match_still_wins_over_a_content_description` | the drawn active label still wins (`Home` → `(403,2232)`) |
| `an_off_screen_node_is_never_a_tap_target` | a warm-mounted page at x=+displayWidth is not a target by text **nor** by content description, and `find_invisible` still sees it (so `ELEMENT_OFFSCREEN`, not `ELEMENT_NOT_FOUND`) |

One earlier run of the same command stopped at `acos-art
 tests/launch_hang.rs::guest_exits_before_milestone_returns_exit_code_and_cleans_up`
("launch took 9.89 s to report a guest that had already ended (bound: 5 s)") while
another agent's benchmark held the host; the re-run above has that target green in
0.87 s. It is a latency bound on the launch path, which this track does not touch —
recorded, not hidden.

## 4. Decisions

* **`visible` is not touched.** It is the framework's `isVisibleToUser()` answer, it
  matches the device (screenshot above), and the honest fix must not paint over it.
* **Input resolution ≠ evidence resolution.** Taps resolve the platform's *accessible
  name* (text, else content_description) among visible nodes; `assert_text`,
  `assert_visible`, `wait_for` and every assertion keep matching the exact field, so
  nothing that is currently a failure can become a pass (`negative/*-wrong` all still
  fail at their own step, `offscreen-pager-visible` still `ELEMENT_OFFSCREEN`).
* **No ancestor-climbing.** The dispatch asked whether tap-by-text should resolve to the
  “nearest clickable ancestor”; measured against this tree that rule does not work and is
  not needed: neither invisible `Settings` label has a clickable ancestor at all (their
  ancestors are two plain `ViewGroup`s), and “tap the centre of the first invisible
  match” would land at x=474, a dead gap between two tabs — a *wrong* tap, not a fix.
  UiAutomator's own `UiObject.click()` clicks the visible-bounds centre of the matched
  node; the platform name is what makes the match reach the right node.
* `acos_protocol::ui::UiTree::find_input_target` is the single place the rule lives, so
  every backend that resolves by text gets the same semantics.

## 5. Status of the test suite / remaining

* `cargo test -p acos-cli -p acos-art -p acos-mcp -p acos-verify`: **525 passed, 0 failed**
  (`.cache/rntap/cargo-test2.log`), see §3 D for the one host-load flake.
* Not covered on purpose: the second copy of the labels (`…1.1.0.5.0.*`, the animated
  row) is invisible and stays unused; if react-navigation ever draws those instead, the
  text match wins again and the rule still resolves the drawn label.
* The phone was **not** used for a fresh dump: the device is on its lock screen
  (`uiautomator dump` returned only the keyguard — `.cache/rntap/phone/tabs-nodes.json`,
  `.cache/rntap/phone-tabs.py`), and unlocking is a security change I am not authorised
  to make. The device side of this finding is the recorded PHONE2 screenshot of the same
  screen; everything else is the guest's own framework answer (the same code a device
  runs) plus the APP-visible tab pressables that the PHONE2 replay taps successfully.
