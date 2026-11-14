# SE2 handoff — `settle: content` (ADR-0026 decision 3), and Gaten Backlog G3e

Branch `settle-se2-content-stable` (worktree `/home/Qui/.paseo/worktrees/3oxa6xao/k5`,
base `main` `0c0b322`). Track SE2. Date 2026-11-14.

## 1. What shipped

Strict stays the default everywhere. New opt-in settle rule **`settle: content`**
for a screen that animates forever in pixels while its UI tree content is stable
(Gaten's welcome/onboarding, GR3 §4.3 / backlog G3e):

* **signature** — `acos_verify::settle::content_signature`: a hash over, in
  visible/tree order, each node's `class`, `text`, `content_desc`, `hint`,
  `checked`, `selected`, `enabled`, `focused`, `bounds`, `visible`, plus the
  child structure/order. Nothing else is hashed and no field is silently
  dropped from that list. A text value an animation updates (a countdown, a
  progress percentage) therefore changes the signature → the screen is not
  content-stable and the step is a `TIMEOUT` (correct answer, no false pass).
* **window** — `ContentWindow`: settled only when the same signature is seen
  across **at least 2 traversals** spanning **≥ 300 ms**
  (`DEFAULT_CONTENT_WINDOW_MS`; configurable upward with
  `ACOS_CONTENT_SETTLE_WINDOW_MS`, never below 300). A different signature
  restarts the window *and* the traversal count, so A→B→A is never a pass.
  The step's `settle_timeout_ms` bounds the wait (default 10 s); running out is
  `TIMEOUT` with `details.reason:"content-not-stable"` and the last signature.
* **where** — scenario `tap`/`type`/`key`/`launch`; serve/MCP
  `acos_observe`/`acos_observe_tree`/`acos_launch`/`acos_reset`/`acos_tap`/
  `acos_text`/`acos_key`/`acos_action`. `acos_assert` needs no settle argument
  after a content step: it evaluates the **content-settled bound tree** without
  a strict re-traversal (which would only time out on that screen).
* **pixels fail closed** — content stability is not pixel evidence. In
  `acos verify`, `assert_screenshot`/`assert_region` bound to a content-settled
  frame fail with `ASSERTION_FAILED`, `details.reason:"content_settle_pixel"`,
  `details.pixel_evidence:false` and the content record, *before* any capture.
  In serve, `pixels:true` with `settle:"content"` is `INVALID_ARGUMENT` (same
  as fast). A fresh strict `capture` rebinds the frame and pixel assertions
  work again.
* **provenance** — the content record is the step's `settle` object and travels
  with every observation it backs:

  ```json
  {"mode":"content","settled":true,"bound":"content-stable","window_ms":300,
   "traversals":17,"signature":"fd74893981ca9367","frame_seq":20,
   "elapsed_ms":751,"pixel_evidence":false,
   "signature_fields":["class","text","content_desc","hint","checked","selected",
                       "enabled","focused","bounds","visible"]}
  ```
* **fail closed if unapplied** — a step that asks for content and gets no
  content record is `UNSUPPORTED_ASSERTION`
  (`reason:"content_settle_unhonored"`); no backend can silently downgrade the
  rule.
* **`settle: fast` unchanged** (navigation only, never backs a pass);
  `expect_change: true` (a pixel claim) with `settle: content` is
  `INVALID_SCENARIO`, exactly like fast.

### Naming

`content` (`StepSettle::Content`, `"content"` on the wire) follows the existing
mode vocabulary (`strict`/`fast`) and the owner decision's own words
("content-stable settle", "UI tree content signature"). The rule name says what
is proven: *content*, not pixels.

## 2. Implementation note: how the traversals carry pixels

A content step must not turn a forever-animating screen into a TIMEOUT just to
get a tree. The guest only knows `strict`/`fast`, and a fast tree is tree-only
(no PNG: P3b refuses fast + capture). So each content-settle traversal is a
**strict tree request with a 1 ms deadline**: the guest answers with the latest
tree *and its PNG* and `settled:false` (P12b's documented timeout observation),
which is exactly what the rule needs — real pixels bound to the traversal the
signature came from, no quiet-window wait. The host's content window is the
verdict; the host replaces the guest's `settled:false` block with the content
record before publishing it, so no caller ever sees a "timed out but content
stable" mixture. A content `launch` binds its tree exactly like a strict launch
(`bind_first_frame` skeleton retry), then runs the content window; its frame is
marked `pixel_evidence:false` and no PNG is claimed for it.

## 3. Code

* `crates/acos-protocol/src/scenario.rs` — `StepSettle::Content`, `LaunchStep.settle`,
  `validate_expect_change` rejects content+expect_change.
* `crates/acos-verify/src/settle.rs` (new) — `content_signature`,
  `ContentWindow`, window floor/`content_window_ms`, unit tests.
* `crates/acos-verify/src/backend.rs` — `SessionObservation.settle`,
  `Backend::set_input_content_settle`.
* `crates/acos-verify/src/executor.rs` — content rule forwarding per input kind,
  bound-settle ledger, `content_settle_pixel_failure`, launch settle plumbing,
  `step_result.settle` from the bound observation (or the launch record).
* `crates/acos-cli/src/engine/art.rs` — `ArtSession::content_settle`,
  `bound_settle`, content launch/reset/observe, content input settle.
* `crates/acos-cli/src/engine/{mod,serve,trust}.rs` — the engine-facing trait
  methods, serve `settle:"content"` on the verbs, content-bound `acos_assert`
  against the cached tree, `pixels:true` refusal.
* `docs/AGENT_API.md` §8.6.4a (+ mode notes in 8.6.1/8.6.3),
  `docs/ERROR_CODES.md` (`content_settle_pixel`, content `TIMEOUT`),
  `crates/acos-mcp/src/descriptors.rs` enums/descriptions.
* `scenarios/gaten-content-welcome.yaml`,
  `scenarios/negative/gaten-content-welcome-wrong.yaml`,
  `scenarios/negative/gaten-content-pixel.yaml`.

## 4. Evidence (real ART, one lock acquisition per batch)

All runs: `run-exclusive.sh --task SE2` wrapping `tools/guarded.sh`, APK
read-only `gaten-dev-f344272-x86_64-release-unsigned.apk`
(sha256 `e04c7ffc…`), scratch `.cache/se2`.

| run | result |
|---|---|
| `gaten-content-welcome` ×3 | **pass 3/3** (`sweep.log` + `content-reps.log`, `suite-content-r{1,2,3}/`); launch, `assert_text` and the `tap` all record `settle.mode:"content"`, `window_ms:300`, `traversals:15`, signature `470c294c658e68bd`; content settle p50 **740 ms** (730/740/743), scenario wall p50 3.11 s |
| `negative/gaten-content-welcome-wrong` ×3 | **fail / `ASSERTION_FAILED`** 3/3 at the content-settled assertion |
| `negative/gaten-content-pixel` ×3 | **fail / `ASSERTION_FAILED`** 3/3, `details.reason:"content_settle_pixel"` (structured fail-closed) |
| 15 gates | exactly the expected verdicts (`sweep.log`): 10 pass; T03-render-wrong, w7-insets-carve-negative, negative/se1-expect-change-noop `ASSERTION_FAILED`; negative/offscreen-pager-visible `ELEMENT_OFFSCREEN`; p11-progress-timeout `TIMEOUT` |
| Gaten flows ×3 | **15/15 pass** (home/tabs/settings/search/lesson ×3) |
| 5 Gaten negatives | **5/5 `ASSERTION_FAILED`** |
| GR4 | support/kanji/recognition/backstack **pass**; their 4 negatives **`ASSERTION_FAILED`** |
| strict control on the welcome screen | launch strict pass, `capture fresh` **`TIMEOUT`** after 1555 ms (`reason:"newer-store"`) — the honest non-pass content settle replaces |

## 5. Tests

`cargo test --release --offline -p acos-verify -p acos-cli` — green (87 acos-cli
bin tests + 56 acos-verify lib tests, including `settle::tests::*` and
`executor::content_settle_tests::*`). `cargo test -p acos-protocol` green after
`UPDATE_SCHEMA=1`. `cargo test --release --offline -p acos-art --
--test-threads=1 --skip refuses_to_run_when_no_cgroup_can_be_created` green
(14 suites, 0 failed, `art-tests.log`).

## 5a. Latency A/B (strict path unchanged)

Interleaved 3 rounds, main arm read-only `.cache/wt-abmain2` (target
`.cache/ab-target`, never built there) vs branch `.cache/k5-target`, one
exclusive acquisition per arm, `--groups "a:latency:8G:300" --latency-reps 12
--reps 1`; per-round p50 (`.cache/se2/ab/{main,branch}-{1,2,3}.json`):

| metric | main | branch | Δ p50 |
|---|---:|---:|---:|
| M4 launch strict | 308.4 / 321.9 / 308.9 | 349.9 / 308.5 / 308.8 | −0.1 ms (−0.0 %) |
| M5 screenshot | 26.4 / 26.9 / 25.0 | 24.6 / 23.8 / 23.7 | −2.6 ms (−9.8 %) |
| M6 reset strict | 335.6 / 335.9 / 332.3 | 335.2 / 337.5 / 348.9 | +1.9 ms (+0.6 %) |
| M7 tree strict | 41.1 / 41.0 / 41.1 | 41.4 / 41.3 / 41.2 | +0.2 ms (+0.5 %) |
| M8 tap counter strict | 967.5 / 950.8 / 954.9 | 958.5 / 953.0 / 951.3 | −1.9 ms (−0.2 %) |
| M8 tap form strict | 944.4 / 949.3 / 941.3 | 952.4 / 949.6 / 941.4 | +5.2 ms (+0.6 %) |

All M4–M8 strict p50 within +5 % / noise (largest absolute +5.2 ms at ~950 ms,
smaller than that metric's own 8 ms rep spread). Content settle p50 740 ms vs
the strict `TIMEOUT` it replaces.

## 6. Known limits / next

* A content-settled frame carries pixels from the *last* stable traversal (the
  guest's 1 ms strict answer), but `pixel_evidence:false`; no pixel claim is
  allowed from it. A strict capture is the only way back to pixel evidence.
* `assert_region`/`assert_screenshot` on a content-settled screen now fail
  immediately instead of timing out on the animation — the honest, structured
  answer; the negative control pins it.
* The onboarding page's own text nodes report `visible:false` in this build
  (GR4 G4a class), so the scenario asserts the welcome screen's copy. Once the
  visibility classification is aligned, the onboarding pages become assertable
  with the same rule and no ACOS change.
* Content settle is per-step opt-in by design. GR3's `settle: fast` Skip taps
  can be upgraded to `settle: content` when their page text is assertable.
