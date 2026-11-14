# TESTHYG2 — long-path socket in `w50_serve_art`, and legacy-VM test triage

Branch `test-launch-hang-cold` (test hygiene, from `main` c517a76 via the LHFLAKE commit;
`git merge main` brought it to `92b9ed4`). Worktree `/home/Qui/.paseo/worktrees/3oxa6xao/gr4`,
target dir `.cache/gr4-target`, evidence `.cache/thyg2/`.
`results/rn/w162a-rn-session.json` was left alone and never staged.

**No product behaviour changed.** Both tasks are test-only; one real product-facing
finding is reported in §3.4 with the reason it needs no code change.

---

## 1. SOCKLEN — the serve socket no longer lives under the worktree

### Symptom and cause (reproduced, not assumed)

`crates/acos-cli/tests/w50_serve_art.rs` built its daemon socket as
`<worktree>/.cache/w50-test-<tag>-<pid>/serve.sock`. `AF_UNIX` addresses live in a
fixed 108-byte `sun_path` (107 bytes + NUL), so the test passed in a short checkout and
failed in a longer one. G3F1 hit exactly that
(`results/handoff/G3F1.md` §3, 2026-11-14): its worktree measured
`<WT>/.cache/w50-test-unknown-session-<pid>/serve.sock` at **108 bytes**, where `bind`
fails, and `an_unknown_session_is_not_routed_to_the_frozen_rust_vm` failed for a reason it
does not assert about (`acos_serve` could not bind → the test's "socket never appeared").

Measured here (`.cache/thyg2/logs/socklen-proof.txt`, driver `.cache/thyg2/socklen-proof.py`),
against a real directory tree, kernel verdict included:

| path length | `bind()` result |
|---|---|
| 107 bytes | **OK** |
| 108 bytes | **`AF_UNIX path too long`** (`OSError`) |

Old-formula length = `len(worktree) + 17 + len(tag) + 1 + len(pid) + 11`, so for the
`unknown-session` tag it fits up to a 57-byte worktree and breaks from 58 bytes:

| worktree | old socket path | result |
|---|---|---|
| 39 (this worktree) | 89 | ok (that is why it is green here) |
| 50 | 100 | ok |
| 57 | 107 | ok |
| 58 | **108** | `ENAMETOOLONG` — G3F1's measurement |
| 65 | 115 | `ENAMETOOLONG` |
| 80 | 130 | `ENAMETOOLONG` |

### Fix (test-only)

New `crates/acos-cli/tests/common/mod.rs`:

* `ShortSocketDir::new(tag)` binds in
  `$XDG_RUNTIME_DIR/acos-test-<pid>-<seq>-<tag>` (fallback `/tmp` **only** when
  `XDG_RUNTIME_DIR` is unset or not a directory), `create_dir_all`, removed on `Drop`.
* `socket(name)` **asserts** the `sun_path` budget (`<= 107` bytes) before use, so a
  future longer tag or base turns into a one-line assertion instead of an
  `ENAMETOOLONG` raised inside `acos serve`.
* Two unit tests: `socket_paths_stay_inside_sun_path` (fits whatever the tag; dir exists
  and is removed on drop) and `a_name_over_the_budget_is_rejected` (`#[should_panic]`,
  so the budget assertion has teeth).

Only sockets moved; the runtime dir, cache dir and logs stay under `<worktree>/.cache`
(the host rule that test scratch lives on disk, not on a tmpfs).

Same pattern fixed in the siblings that built sockets under the worktree:

| file | before | after |
|---|---|---|
| `tests/w50_serve_art.rs` | `dir.join("serve.sock")` (3 daemons + `s.sock`) | `ShortSocketDir` |
| `tests/w55_dead_guest.rs` | `dir.join("serve.sock")` | `ShortSocketDir` |
| `tests/gl1_kept_session.rs` | `dir.join("s.sock")` with a hardcoded `/home/Qui/Projects/acos/.cache/gl1` scratch root as the workaround | `ShortSocketDir`; the hardcoded home path is gone, scratch now `<repo>/.cache/gl1-tests` (`ACOS_GL1_SCRATCH` still wins) |
| `tests/serve_daemon.rs` | `std::env::temp_dir()/acos-w91a-serve-<pid>/session.sock` | unchanged — `/tmp` path is short and independent of the checkout; no failure mode |

### Proof it is path-length independent now

* Path actually used, observed live during 400 back-to-back runs of the failing test
  (`.cache/thyg2/logs/live-socket-paths.txt`):
  `/run/user/1000/acos-test-2364803-0-unknown-session/serve.sock` = **61 bytes**,
  400/400 runs green, **0** directories left behind in `/run/user/1000` after the loop.
  Worst case for this helper is base + 11 + 7 + 3 + 1 + 20 + 11 ≈ **66** bytes — it does
  not contain the worktree path at all.
* `cargo test --release --offline -p acos-cli --test w50_serve_art`
  (**`.cache/thyg2/logs/w50-w55-after.log`**, under `run-exclusive.sh --task TESTHYG2` +
  `guarded.sh --mem 10G`): `5 passed; 0 failed; 1 ignored`
  (`art_agent_loop_over_the_socket` stays ignored, it boots real ART).
  `w55_dead_guest`: `5 passed; 0 failed`.

### Product-side note (reported, no change)

`acos serve --socket <too long>` already fails closed with a clear message —
`serve.rs:116-120`: `acos serve: bind {path}: {os error}` and a non-zero exit — and the
product's *default* socket is `runtime_dir()/session.sock` (`acos-mcp/src/daemon.rs:60`),
i.e. under `$XDG_RUNTIME_DIR`, never under the checkout. The over-long path was built by
the **test**, from a directory whose length the test does not control. `acos-art` solved
the same class of problem for its own guest sockets in `launch.rs:2292`
(`ShortSocketDir`, W41). No product change was made.

---

## 2. LEGACYVM — the seven failures, one at a time

Environment: `.cache/boot/android-34` boot dir, `fixtures/build`, host `javac`/`d8`.
Every run under `run-exclusive.sh --task TESTHYG2` + `guarded.sh --mem 10G`.

| test | cause | first bad | shared code? | action |
|---|---|---|---|---|
| `acos-vm` `inflate_layout_through_real_layoutinflater` | the fixture's `TestContext` extends the real `android.test.mock.MockContext`, whose `isRestricted()`/`canLoadUnsafeResources()` **throw `UnsupportedOperationException` by design**; `TextView.<init>` → `readTextAppearance` asks both | `7248e31` (2026-11-10) — the commit that introduced the test with this fixture; red ever since (`results/perf/BOOTIMAGE_CACHE.md` §6 reproduced it on a clean `b845c69`) | no (test-side Java fixture) | **fixed, test now green** |
| `acos-runtime` `display_manager_reply_round_trips_through_the_real_java_creators` | asserts the pre-W7 **carved** `DisplayInfo` app bounds `1080x2209`; W7b made `report` the default inset model, which reports `appBounds` as the full display `1080x2400` | **`18622e5`** (2026-11-14, "W7b: window insets default report (ADR-0026 #6)") | **yes — acos-wm + acos-services, intentional** | `#[ignore]` |
| `acos-runtime` `add_to_display_as_user_returns_reference_frame_and_insets` | asserts the carved `outFrame (0,128,1080,2337)`; under `report` the app window is the full display and the bars are only *reported* as insets | **`18622e5`** | **yes — acos-wm** | `#[ignore]` |
| `acos-runtime` `relayout_returns_a_valid_surface_control_and_reference_frames` | same: `frames.frame`/`surface` carved vs full display (`0,0,1080,2400`) | **`18622e5`** | **yes — acos-wm** | `#[ignore]` |
| `acos-runtime` `tap_outside_every_window_is_a_structured_error` | a tap at `(5,5)` used to fall outside the carved window (frame starts at `y=128`); under `report` the window covers the whole display, so the tap legitimately hits it (`inject_tap` returns `Ok`) | **`18622e5`** | **yes — acos-wm / acos-services wms** | `#[ignore]` |
| `acos-runtime` `list_scroll_button_moves_the_recycled_window` | the bounded vsync frame budget added for the V1 verify loop stops `AbsListView.smoothScrollToPosition` advancing inside the pump: the tap reports `state_changed` but the pump's tree still holds the old rows (it passed before only by chasing the animation to completion, 125 s) | **`f0c9fdb`** (2026-11-11, W191-A, "vsync frame budget"); documented in `results/v1/W191A.md` §4 | no (`acos-runtime/src/lifecycle.rs` + `acos-vm/src/natives/fwk_vsync.rs`, both legacy) | `#[ignore]` |
| `acos-runtime` `dialogs_alert_opens_and_cancel_dismisses` | `AlertDialog`'s button-bar `Button` inflation dies in `AnimatorInflater.loadStateListAnimator` → `Resources.getAnimation` → `Resources$NotFoundException: Resource ID #0x0`: the theme attribute `View_stateListAnimator` does not resolve, so `0` reaches the real framework code. Legacy-VM resource/theme gap | **`7248e31`** (2026-11-10) as far as the record goes: W187-A saw it PASS (`results/app-matrix/W187A.md` §2a) and W191-A saw it fail at *its own* base `de66a09` (2026-11-11, `results/v1/W191A.md`), so the window is `e206287..de66a09` with `dbaf79b` (W198-A, "static RRO overlays applied in AssetManager", i.e. changes which *framework resource values* a theme lookup sees) the strong candidate; the frozen-VM tree could not be bisected commit-by-commit inside the budget (each step is a full `acos-vm` rebuild) | no (`acos-vm/src/natives/fwk_res.rs`; `acos-framework-natives` is a dependency of `acos-vm` only — `acos-art` does not use it) | `#[ignore]` |

### 2.1 `inflate_layout` — fixed, not ignored (test-only)

Adding two overrides to the fixture's `TestContext` (`crates/acos-vm/tests/java/InflateMain.java`)
is the faithful fix, because a real app's context is an `Activity`/`ContextWrapper`, whose
`isRestricted()` is `false` and `canLoadUnsafeResources()` is `false`; `MockContext` answers
"unknown" by throwing, which is AOSP's documented mock behaviour:

```java
@Override public boolean isRestricted() { return false; }
@Override public boolean canLoadUnsafeResources() { return false; }
```

The first override alone moved the failure to `canLoadUnsafeResources` — which also proves
the VM's `invoke-virtual` dispatch to the subclass override is correct, so T1's guess
("the framework call box still resolves `MockContext.isRestricted`", `results/tests/T1.md`
T1c) was wrong. **No assertion was touched**; the tree needles
(`LinearLayout`/`TextView id=canvas_status`/`text=tap the canvas`/`ShapeView id=shape`) are
unchanged, and `cargo test -p acos-vm --test inflate_layout` is now **1 passed**.

### 2.2 Why the six ignores are right, and why the four geometry ones are *drift*

The inset-model change is a deliberate, documented, AOSP-faithfulness change in **shared**
code — `crates/acos-wm/src/display.rs:28-47` (`ACOS_WM_INSETS=carve|report`,
`DEFAULT_INSETS_MODE = Report`) and `crates/acos-services/src/display.rs:501-513`.
ADR reference: the commit itself names **ADR-0026 #6**; the enum's own doc says
`Carve` is "pre-W7: the window manager subtracts the system bars from the frame" and
`Report` is "AOSP: full-display window plus a reported `InsetsState`".
W7 (`a6610a8`) introduced the switch with `Carve` still the default; **W7b (`18622e5`)**
flipped the default to `Report` and re-blessed the shared-path expectations
(`acos-services/tests/{displayinfo_api34,window_session_api34,window_session_framework_order,w32_dialog_window}.rs`,
`acos-verify/src/display.rs`) — but not the frozen-VM tests in `crates/acos-runtime/tests/`,
which had not been touched since 2026-11-10/11 (`40f075c`, `1041c21`). So the four
geometry failures are exactly "the shared default moved, the legacy expectations did not".

Consequences, stated plainly:

* **Shared-code impact: the product is not broken.** The report model is the intended
  behaviour for the ART path and is covered by the re-blessed `acos-services` tests; the
  frozen-VM wire tests still encode the device *carve* capture
  (`0,128,1080,2337`, app `1080x2209`). Re-blessing *those* would change what the frozen
  VM asserts about a legacy route, and forcing `ACOS_WM_INSETS=carve` would hide the
  default — both are product/architecture decisions, not test hygiene. `#[ignore]` with the
  cause and the commit is the honest state.
* The frozen policy that makes "preserve the test, do not chase feature work" the right
  call is explicit: `docs/ENGINEERING_POLICY.md` ("Rust VM and its tests are preserved,
  feature development frozen") and `docs/OPERATING_DIRECTIVES.md` §4 ("Rust VM (frozen, no
  feature work)"). No assertion was deleted or weakened anywhere: every ignore keeps its
  body verbatim and names the cause, the commit and `results/handoff/TESTHYG2.md`.

### 2.3 Ignore reasons (verbatim, as they render in `cargo test`)

* wms `add_to_display_as_user_returns_reference_frame_and_insets`: *frozen legacy Rust VM:
  asserts the pre-W7 carved outFrame (0,128,1080,2337); W7b (18622e5) made report the default
  inset model, ADR-0026 #6, so the app window is the full display — see results/handoff/TESTHYG2.md*
* wms `relayout_returns_a_valid_surface_control_and_reference_frames`: *frozen legacy Rust VM:
  asserts the pre-W7 carved ClientWindowFrames (frame 0,128,1080,2337, surface 1080x2209);
  W7b (18622e5) made report the default inset model, ADR-0026 #6, so the window is the full
  display — see results/handoff/TESTHYG2.md*
* input `tap_outside_every_window_is_a_structured_error`: *frozen legacy Rust VM: a tap at
  (5,5) is inside the app window since W7b (18622e5) made report the default inset model
  (ADR-0026 #6): the window is the full display and the bars are only reported as insets —
  see results/handoff/TESTHYG2.md*
* display `display_manager_reply_round_trips_through_the_real_java_creators`: *frozen legacy
  Rust VM: asserts the pre-W7 DisplayInfo app bounds 1080x2209; W7b (18622e5) made report the
  default inset model (ADR-0026 #6), which reports appBounds as the full display 1080x2400 —
  see results/handoff/TESTHYG2.md*
* app_matrix `list_scroll_button_moves_the_recycled_window`: *frozen legacy Rust VM: the
  bounded vsync frame budget added in f0c9fdb (W191-A) stops
  AbsListView.smoothScrollToPosition advancing inside the pump; the tap reports state_changed
  while the tree still holds the old rows — see results/handoff/TESTHYG2.md*
* app_matrix `dialogs_alert_opens_and_cancel_dismisses`: *frozen legacy Rust VM: AlertDialog's
  button-bar Button inflation throws Resources$NotFoundException Resource ID #0x0 from
  AnimatorInflater.loadStateListAnimator — the theme attr View_stateListAnimator is not
  resolved, a legacy-VM resource/theme gap (open since 7248e31, 2026-11-10) — see
  results/handoff/TESTHYG2.md*

---

## 3. Acceptance

```
# TASK 1
run-exclusive.sh --task TESTHYG2 -- guarded.sh --mem 10G --time 900 -- \
  cargo test --release --offline -p acos-cli --test w50_serve_art
→ ok. 5 passed; 0 failed; 1 ignored        (.cache/thyg2/logs/w50-w55-after.log)
   (+ w55_dead_guest: 5 passed; 0 failed)

# TASK 2 — both crates, every target, nothing filtered
run-exclusive.sh --task TESTHYG2 -- guarded.sh --mem 10G --time 1500 -- \
  cargo test --release --offline -p acos-vm -p acos-runtime --no-fail-fast
→ ok. 330 passed; 0 failed; 10 ignored      (.cache/thyg2/logs/full-vm-runtime.log, 1m32s)
```

The 10 ignores in `acos-runtime` = the 4 pre-existing "OPEN" `app_matrix_interaction`
cases (untouched) + the 6 justified here; `acos-vm` has 0. Nothing is skipped silently:
each ignore carries its cause in the attribute, and the previously-green tests in the
same binaries still run (`app_matrix_interaction`: 4 passed; `wms`: 6 passed; `input`:
2 passed; `display`: 0 run — that binary holds only the ignored case).

### Evidence files (`.cache/thyg2/`)

| file | content |
|---|---|
| `socklen-proof.py`, `logs/socklen-proof.txt` | old path length per worktree length + kernel `bind` at 107 (ok) / 108 (`AF_UNIX path too long`) |
| `logs/live-socket-paths.txt` | 400 live runs' socket paths (61 bytes), 0 leftovers |
| `logs/w50-w55-after.log` | TASK 1 acceptance run |
| `logs/display-before.log`, `logs/wms-input-before.log`, `logs/appmatrix-before.log` | the pre-fix failures quoted above |
| `logs/targeted-after.log` | the four affected `acos-runtime` binaries green with the ignores |
| `logs/full-vm-runtime.log` | full `acos-vm` + `acos-runtime` suites |

---

## 4. Peak resources

* RAM: `guarded.sh --mem 10G` for every test run, `--mem 12G --vmem 40000000` for the
  builds; host MemAvailable never fell below 34 GiB (`free -g`: 39 GiB available at the
  end), i.e. ~35 % used — well inside the 90 % ceiling. Largest single guest scope peak
  observed: 6.3 GB (`memory.peak` of the guarded scope during the full-suite run).
* Disk: `/` 83 % at start, **84 %** at the end (stop line 85 %). This work added no new
  build target (reused `.cache/gr4-target`, 12 GB, pre-existing) and 412 KB of evidence;
  nothing under `WT/out` to prune.
* No sudo, no network, no new worktree; scratch on disk (`.cache/thyg2`), `/tmp` used only
  by the pre-existing `serve_daemon.rs` socket dir and by nothing this task added.
