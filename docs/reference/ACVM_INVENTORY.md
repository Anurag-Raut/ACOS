# ACVM legacy inventory (read-only audit)

Status: **research artefact, 2026-11-10.** Owner: ACOS docs/reference.
Source of truth: `/home/Qui/Work/android-container` at `main` = `d005a0d`
("docs: ACVM Lite direction (own runtime), AOSP track archived"), read-only.

Nothing in this document was executed against the legacy engine: no container
start/stop, no Docker, no benchmarks, no git mutation. Facts come from reading
the tree, `git log`, `git status --short`, files, and `aapt2 dump badging`
against already-built APKs under `helper/build/`.

`git status --short` was empty at the start and end of this audit.

---

## 0. Orientation

| Item | Value |
|---|---|
| Repo | `/home/Qui/Work/android-container` |
| `main` | `d005a0d` (== `origin/main`) |
| AOSP-track freeze | tag `aosp-track-final` = branch `archive/aosp-track-2026-11-10` = `31da75a` |
| `main` diff vs. archive branch | exactly the new file `research/acvm-lite-direction-2026-11-10.md` |
| Engine model | one privileged **redroid** container ("core") + one Android **user** + one `TRUSTED\|OWN_FOCUS` virtual display + one app per **session** |
| Average session | one Android `UserHandle` + one app + one display; `acvm` CLI / MCP; RAM guard 85 % |

The legacy engine is a complete, measured *reference implementation*. It is
**not** the ACOS architecture and, as of `d005a0d`, it is explicitly frozen
(see §1, item D1).

---

## 1. Reconciled status vs. `docs/handoff/05_OLD_ACVM_REFERENCE.md`

The handoff was written from a user-supplied summary. The tree has moved. The
table reconciles every material claim in handoff §"Latest reported legacy work
state" and the "Major verified discoveries" list.

| # | Handoff claim | Actual state in tree (evidence) | Discrepancy |
|---|---|---|---|
| D1 | *(not mentioned)* | **Latest commit `d005a0d` records a user decision to build an "own lightweight Android testing runtime" ("ACVM Lite") and archives the AOSP track** (`research/acvm-lite-direction-2026-11-10.md`; tag `aosp-track-final`). Proposed Lite architecture: APK → standalone ART → minimal Android API layer → host graphics + native libs. | **Major, unmentioned by the handoff.** The legacy engine is now frozen as of `31da75a`; the strategic direction is a from-scratch runtime — which overlaps directly with ACOS. Treat ACVM as a *frozen behavioural/benchmark oracle*, not a live product track. |
| D2 | "v12, D2 and task-first plan merged/pushed; 281 agent and 138 bench tests passed" | v12 `7565d76`, D2 `b1b0a58`, task-first `a6701e8`/`fb8f8cd` all on `main`. D2's own report says **272 agent**, ITERATIONS D2 row says 281 agent; later W5 merged at **308 agent + 138 bench**. | Test count stale/ambiguous (272 vs 281 vs 308); use 308/138 as the latest accepted main state. |
| D3 | "W5 merged; W4 neutral not promoted" | W5 `d3a3661`/`31da75a` on `main`; W4 reports `NEUTRAL`/`preload trim REJECT` and is "NOT merged" (`ITERATIONS.md`, `iter/V10.md` refs). | Consistent. W4 image kept as opt-in candidate only. |
| D4 | "W1 ~98 MiB bare-core saving accepted, rebased to v11; promotion pending" | W1 A/B measured on base `acvm-core:v9`: bare workset **303.9 → 206.0 MiB (−97.9)**, `memory.current` −124.2; N=1 0.0, N=5 −1.2. W1 code merged to `main` (`9da83b2`, `61e7060`, `1dda635`, `4cc5cbd`). Default switch still pending a U1 gate on a v11-based image (`w1v11`); W1's own report says the U1 gate was *not yet* run on the branch. | Mostly accurate. Note "rebased to v11" is a follow-up plan; the measurement base was v9. |
| D5 | "R1 host renderer merged as research; warm ~6.3 s, peak 1.26 GiB" | R1 merges `931e71f`/`bfc56cc` + row `2f7392b`. `iter` R1 row: warm p50 **6.27 s** (p95 6.69), peak RSS **1.26 GiB**, 380 MiB deps, 8/8 positives, 3/3 seeded defects, 0 false passes, 3/3 unsupported refused. | Consistent. |
| D6 | "R2, D3, D4, U2, W2b still in progress" | **U2 implementation is NOT merged to `main`**: `74c9dc9`, `559c652` (U2 devloop bench) are only on branch tips. **R2 is NOT merged**: `5320ebc`, `dd604c4`, `deac7d5` only on `research-taskfirst-r2`. **W2b fix** `66a2d34` only on `mem-w2-softrender`. **D3** `adbca60` only on `devloop-d3-observe-dexopt`. **D4** `bbd2392` "WIP snapshot at stop" only on `devloop-d4-toolkit`. | Handoff correctly says in-flight, but be precise: these are **branch work, not on `main`**. `main` contains only W1/W4/W5/R1-report/v12/D2/task-first docs. |
| D7 | "Old five-session count is a benchmark point, not an intrinsic limit; ten extra displays created" | `E3.2`: overlay displays capped at 5 by AOSP `OverlayDisplayAdapter`; `P1.5`: **10 extra public virtual displays**, 0 failures. | Consistent. The cap was overlay-display-specific. |
| D8 | "v1 persistent frames: screenshot ~431 → ~67 ms; resume ~215 ms" | ITERATIONS v1: shot **0.066 s**, resume→shot **0.215 s** (v0 0.431). | Consistent. |
| D9 | "v9 N=5 ~79 MiB incremental (anon+shmem), N=1 ~130 MiB" | ITERATIONS v9: N=1 **130.3**, N=5 **78.7** MiB (workset p50). | Consistent. |
| D10 | "W7 shared-process tasks +11–22 MiB per extra view" | `iter/W7.md`: 10.8–21.9 MiB vs 85.3–89.1 MiB for a user session at N=2..5, but the shared bed was **31.1 MiB above** the user bed, so the corrected quote is **≈21–29 MiB/extra task at N=5**. | Handoff under-quotes the honest marginal. Use 21–29 MiB with the caveat. |
| D11 | "v5–v12; after v12 ~3.62 s median, guest boot ~1.5 s in a prior stage" | `iter/V12.md`: interleaved n=5 cold **4.260 → 3.620 s (−0.640 s)**, every pair ≥0.55 s faster. Guest boot: v10 **1.517 s** p50. | Consistent; these are different metrics (cold path vs guest `boot_completed`). |
| D12 | "D2 reset `--app` ~0.11 s cmd, ~0.75 s to first fresh frame (12 runs); 46–50 s was first text tree after new-user observe dexopt" | `iter/D2.md`: p50 **110.3 ms** cmd / **748.1 ms** to first non-black frame (p95 114.7/768.6, n=12). U1 step-11 breakdown: reset 0.4 s + launch 0.1 s + shot 0.6 s + `ui --text` **56.5 s** (observe dexopt for the new user). | Consistent. |
| D13 | "W5 park compaction ~56 MiB PSS; frozen tier passed a 10-min soak" | `iter/W5.md`: compaction +39.9 MiB PSS over detach (detach ~16.0 → ~56 MiB PSS freed, state kept, resume→shot +0.003 s); frozen+page-out 54.6 MiB PSS; 10-minute binder-safety soak **PASS**. | Consistent; "~56 MiB" is PSS freed from a pristine app, not incremental over detach. |
| D14 | "W2 software render: -47 %/-57 %, +120 MiB bare, +7.5 s boot" | `iter/W2.md`: N=1 **129.1 → 67.9 (−47.4 %)**, N=5 **78.1 → 33.2 MiB (−57.5 %)**; +7.5 s first boot, +119.8 MiB bare (boot-image checksum fallback). Net worse at N=1 (+58.6 MiB kernel), better at N≥2.7. | Consistent; note the net kernel-memory trade-off, not just per-session. |
| D15 | "KSM powerful opt-in, timing side channel" | `v6` KSM N=1 18.4 / N=5 40.4 MiB workset vs v5 136.5/105.7; opt-in `ACVM_KSM`, default OFF (`iter/V6.md`). Earlier P3 accounting was contradictory (`EXPERIMENTS.md` §11, §18). | Consistent, with the honest caveat that the saving is cgroup-accounted and rides a timing side channel. |
| D16 | "system_server/ART floor, ~200 MiB never proved a lower bound" | `P2c` 278 MiB bare-verified; `W1` 206.0 MiB bare on v9. Bare reads span 259–322 MiB across host states (`EXPERIMENTS.md` §24). | Consistent. **Bare-core numbers are only comparable within one benchmark session.** |
| D17 | "Privileged redroid fails the untrusted-APK security gate" | `docs/security/REVIEW.md` verdict: fails for untrusted APKs, acceptable only as a trusted single-tenant research setup; `--privileged` container root == host root (privilege matrix). | Consistent. |
| D18 | handoff expected `spikes/`, `runtime/` | Both **ABSENT** from the checkout. `runtime/` is the default `ACVM_RUNTIME` for the adb/service backends; `spikes/` is cited by `EXPERIMENTS.md` (e.g. `spikes/p2/results/strip-steps.tsv`). | Referenced paths no longer exist on `main`; historical evidence is not reproducibly present. |
| D19 | handoff lists a 23-app compatibility corpus | `iter/compat-v2.json` holds **10 F-Droid apps** (not 23) under `acvm-core:s12-nolowram`; P2b text claims 21/23 in 8 classes. The APK files themselves are **not in the repo**. | The 23-app corpus is not present as fixtures; only a 10-app result JSON survives. |

### Status one-liner

`main` is a frozen, heavily measured redroid/ACVM engine with boot/memory/reset
levers accepted (v1–v12, W1–W8/D1/D2/U1/W2/W3/W5/W7), a research host renderer
(R1), and a task-first benchmark plan. As of the latest commit it is **frozen**
(`aosp-track-final`) while the user redirects to an own-runtime "ACVM Lite"
investigation that closely parallels ACOS.

---

## 2. Measured legacy numbers

**Metric definitions (from `bench/lib/harness.py`, `bench/score.py`):**

- **workset** = cgroup `anon + shmem` of the core's cgroup, MiB (`cgroup_mem`).
- **memory.current** = cgroup v2 `memory.current` of the core, MiB.
- **PSS** = sum of per-process `smaps_rollup` PSS over every pid in the core
  cgroup, MiB (`cgroup_pss_mib`) — shared pages counted once.
- **host_delta** = `MemAvailable_before − MemAvailable_after`, MiB.
- **per-session memory N=k** = `(loaded − bare) / k`, MiB; `bare` is the settled
  core before sessions; `marginal` = `(total(N=10) − total(N=5)) / 5`.
- **cold core-ready → first fresh shot** = container create → first
  `fresh && non_black` screenshot (includes helper install/framework restart and
  session create), seconds (`Bench.measure_cold`).
- **create session → fresh screenshot** = warm core, `acvm up` create to fresh
  frame, p50 seconds.
- **fresh screenshot** = retrieval latency for a request-sequenced,
  non-stale frame (`seq`/`frame`/`sha256`), p50 seconds.
- **reset → ready** = `reset --app` command latency and to first fresh
  non-black frame, p50 seconds.
- **UI tree** = latency of one tree dump through the stated route.
- All memory in **MiB = 2²⁰ B**; all times in seconds; "cold"/"warm" as noted.

| Metric | Value | Metric type | Cold/warm | Source |
|---|---|---|---|---|
| Stock emulator cold boot | 25.2 s, ~3300 MiB PSS | PSS, boot | cold | `EXPERIMENTS.md` §E1.1 |
| Stock emulator snapshot | 12.1 s, ~3200 MiB | PSS | warm restore | `EXPERIMENTS.md` §E1.1 |
| redroid initial footprint | 753 MiB working set | cgroup ws | cold | `EXPERIMENTS.md` §E2.1 |
| Bare core (stripped, P2c) | **278 MiB** anon | anon | settled | `EXPERIMENTS.md` §5 |
| Bare core v9 (A/B base) | **303.9 MiB** workset / 301.1 anon / 409.9 current | ws/anon/current | settled 60 s | `iter/W1.md` §5 |
| Bare core W1 (diet) | **206.0 MiB** workset / 203.4 anon / 285.7 current | ws/anon/current | settled 60 s | `iter/W1.md` §5 |
| Bare-core read spread across sessions | 259–322 MiB | anon/ws | settled | `EXPERIMENTS.md` §24 |
| Per-session N=1, v9 | 130.3 MiB workset | anon+shmem | warm, 60 s | ITERATIONS v9 |
| Per-session N=5, v9 | 78.7 MiB workset | anon+shmem | warm, 60 s | ITERATIONS v9 |
| Per-session N=5, v8 | 93.2 MiB | anon+shmem | warm | ITERATIONS v8 |
| Per-session N=1 / N=5, v6 KSM | 18.4 / 40.4 MiB | workset | warm, opt-in | ITERATIONS v6 |
| Per-session N=1 / N=5, W2 soft render | 67.9 / 33.2 MiB | workset | opt-in, +7.5 s boot | `iter/W2.md` |
| Per-session N=5, W3 lean 640×360 | 61.4 MiB (−23 %) | workset | warm, opt-in | ITERATIONS W3 |
| Per-session N=5, W3 lean 960×540 | 68.1 MiB (−15 %) | workset | warm, opt-in | ITERATIONS W3 |
| Marginal per session N=5→10 | v5 ~99.3 MiB ws, 112.1 current | ws/current | warm | `EXPERIMENTS.md` §v6 row |
| Shared-process extra task N=5 | ≈21–29 MiB | workset | warm, **no isolation** | `iter/W7.md` |
| App native heap / session (older bed) | 87.6 MiB (84.2 Scudo; one 75,296,768 B pool) | private dirty | warm | `research/native-heap/ANATOMY.md` |
| Cold core→first shot v4 | 18.94 s | wall | cold | ITERATIONS v4 |
| Cold core→first shot v5 | 11.29 s (−40.4 %) | wall | cold | ITERATIONS v5 |
| Cold core→first shot v10 | 7.73 s | wall | cold | ITERATIONS v10 |
| Cold core→first shot v11 | 4.198 s (−40.6 %) | wall | cold | ITERATIONS v11 |
| Cold core→first shot v12 | **3.620 s** (v11 4.260) | wall | cold, interleaved n=5 | `iter/V12.md` |
| Guest boot (sys.boot_completed) v10 | **1.517 s** (v9 4.221) | wall | cold | ITERATIONS v10 |
| Worker create→frame, P5b backend | **1.251 s** p50 (p95 1.329, n=20) | wall | warm | `EXPERIMENTS.md` §21 |
| Worker create→frame, corrected P1.8 | **1.343 s** p50 (p95 1.487, n=30) | wall | warm | `EXPERIMENTS.md` §23 |
| create→shot, v9 scoreboard | 0.524 s | wall | warm | ITERATIONS v9 |
| Launch + first frame, U1 default | 375 ms | wall | warm | `iter/U1.md` §4 |
| Fresh screenshot p50 | **0.066–0.067 s** | wall | warm | ITERATIONS v1/v9 |
| Resume→fresh screenshot | 0.207–0.215 s | wall | warm | ITERATIONS v1/v9 |
| `reset --app` command | **110.3 ms** p50 (114.7 p95, n=12) | wall | warm | `iter/D2.md` |
| `reset --app` → first frame | **748.1 ms** p50 (768.6 p95, n=12) | wall | warm | `iter/D2.md` |
| Whole-user reset → frame | 931 ms p50 | wall | warm | `iter/D2.md` |
| U1 step-11 reset (whole user + text tree) | **~48–50 s** | wall | warm | `iter/U1.md` §4 |
| UI tree, window-manager structure | 14–26 ms p50; 87 ms (P5b) | wall | warm | `EXPERIMENTS.md` §12/§21 |
| UI tree, on-device service | 2.2–2.8 ms | wall | warm | `EXPERIMENTS.md` §E4.2 |
| UI tree, persistent observe instrumentation | **4.1 ms** p50 | wall | warm, +12.6 MiB PSS/session | `EXPERIMENTS.md` §25 |
| UI tree, one-shot observe (`--text`) | 538 ms p50; U1 steady 594–768 ms | wall | warm | `EXPERIMENTS.md` §25, `iter/U1.md` §4 |
| Parked session idle CPU | 3.86–5.94 % whole container (unreliable column) | CPU window | parked | ITERATIONS v0–v9 |
| N-scaling | N=1/5/10 measured; 10 public virtual displays created, 0 failures | workset | warm | `EXPERIMENTS.md` §E3.2/§P1.5 |
| R1 host renderer | warm p50 6.27 s, peak RSS 1.26 GiB, 380 MiB deps | RSS/wall | warm, host-only | ITERATIONS R1 |
| Chrome reference | core 340 MiB PSS; marginal tab 18–24 MiB; shot 61 ms; AX tree 18 ms | PSS | warm | `EXPERIMENTS.md` §13 |

> **Warning carried over from the legacy repo:** never mix PSS, `anon+shmem`
> workset, `memory.current` and host `MemAvailable` deltas as if identical; and
> only compare rows produced in one benchmark session on the same host state
> (`EXPERIMENTS.md` §24).

---

## 3. Fixture corpus inventory

All APKs present in the checkout. Facts from `aapt2 dump badging`
(`/home/Qui/Android/Sdk/build-tools/35.0.0/aapt2`). None of these were installed
or launched. **License status: self-authored debug/test APKs inside the legacy
repo — no third-party licence is declared; reuse as ACOS fixtures requires a
provenance/licence check.** The `vdisp` helper uses the public AOSP *platform
test key*, which is a research-only signing shortcut and must not be reused in
ACOS.

| APK (path) | package | version | min/target SDK | ABI | Exercises | Source present | Reusable as ACOS fixture |
|---|---|---|---|---|---|---|---|
| `helper/build/acbench-tiny.apk` | `com.acbench.tiny` | (unsigned version fields) | aapt2 does not print `sdkVersion`; build.sh `--min-sdk-version 21` | all (Java/dex only) | Minimal `Activity` drawing a continuously changing solid colour (`postInvalidateDelayed(50)`) — launch latency, non-black frame source | yes: `helper/testapps/acbench-tiny/` | **yes** (self-owned, debug-signed) |
| `helper/build/p1probe.apk` | `com.acvm.p1probe` | 1.0 | min 21 (build.sh) / target 34 | all | Views: `TextView`, `EditText` (typed.txt), `Button` (NEXT/SecondActivity, CRASH deliberate), 40-row `ListView`; `ProbeService` foreground ballast; `P1A11yService`; PixelCopy window capture | yes: `helper/testapps/p1probe/` | **yes** (primary UI-loop fixture) |
| `helper/build/webprobe.apk` | `com.acvm.webprobe` | 1.0 | min 30 / target 34 | all | `WebView` with JS/DOM storage, built-in data URL; **platform-signed** | yes: `helper/testapps/webprobe/` | source yes; **re-sign before reuse** (platform key) |
| `helper/build/observe.apk` | `com.acvm.observe` | 1.0 | min 30 / target 34 | all | UiAutomation instrumentation; text/desc/resource-id/bounds tree for a background user's display | yes: `helper/observe/` | **yes** (reference for a text-tree route) |
| `helper/build/vdisp.apk` | `com.acvm.vdisp` | 1.2 | min 30 / target 34 | all | Virtual-display helper: `ADD_TRUSTED_DISPLAY`, `CAPTURE_VIDEO_OUTPUT`, `INTERNAL_SYSTEM_WINDOW`, ImageReader capture with per-request `seq`; **platform-signed** | yes: `helper/vdisp/` | architecture reference only; **do not reuse platform signing** |
| `image/buildctx/acvm-cfg-overlay.apk` | `com.acvm.cfgoverlay` | – | target 34 | all | Runtime-resource-overlay config package baked into the core image | source: `image/overlay/` | build-time image artefact, not a test fixture |
| 23-app F-Droid compatibility corpus (P2b) | e.g. `org.fossify.notes`, `org.fossify.gallery`, `net.sourceforge.opencamera`, `com.stoutner.privacybrowser.standard`, `org.breezyweather`, `com.github.libretube`, `com.termux`, `com.nextcloud.client`, `com.looker.droidify`, `org.fdroid.fdroid` | third-party | varies | mix incl. 3 ARM-only | WebView/Flutter/RN/Compose/native/background/heavy-heap classes | **APKs NOT in repo**; only `iter/compat-v2.json` (10 apps) and prose counts | **no** — re-acquire under licence; the JSON is evidence, not a corpus |

**Fixture gaps for ACOS:** there is no committed Compose fixture APK, no
Flutter/RN/Unity APK, no NDK/JNI `.so` fixture, no multi-DEX/split-APK fixture,
no WebView-only internet fixture beyond the tiny `webprobe`, and no
GMS/permission-dialog fixture in the tree. R1 has Gradle *source* fixtures
(`research/task-first/r1/app/`) for host rendering but no built APK.

---

## 4. Agent contract worth carrying over

Sources: `agent/README.md`, `agent/SCHEMA.md`,
`agent-service/PROTOCOL.md`, `docs/AGENT-LOOP.md`.

### 4.1 CLI surface

`acvm` (thin Python over `acvm_lib.cli`), one JSON object on stdout, logs on
stderr, non-zero exit on failure. Backends selected by `ACVM_BACKEND`:
`adb | service | auto | redroid` with identical output shape.

Commands: `up` (`--count`, `--snapshot`, `--app`, `--apk`, `--image`,
`--profile`, `--display-size`), `core [--up|--down]`, `park [--tier
detach|frozen|app|user]`, `resume`, `reset [--app]`, `tick [--reclaim]`,
`fork --share-process`, `capacity`, `ls`, `down [--all]`, `snap`, `ui [--text]`,
`tap/type/key/swipe/long-press/clear-text`, `shot [--scale]`, `install
[--allow-debug]`, `launch [--component|--intent-action --data --mime --extra]`,
`logcat`, `sh`, `wait`, `assert`, `events`/`errors`. MCP exposes
`acvm_<command>` with the same objects.

### 4.2 JSON envelope & identity

- Success `{"ok": true, ...}`; failure `{"ok": false, "error", "code", "hint"}`
  (exactly those four keys).
- Common error codes: `bad_args`, `missing_instance`, `unknown_instance`,
  `stale_ref`, `unsafe_path`, `ram_guard`, `ram_abort`, `disk_full`,
  `runtime_missing`, `runtime_protocol`, `unsupported_backend`,
  `create_user_failed`, `window_timeout`, `vdisp_timeout`, `shot_failed`,
  `tree_failed`, `no_window`, `ui_failed`, `timeout`, `service_unavailable`,
  `install_failed`, `launch_failed`, `internal`, `core_missing`,
  `shared_process`, `shared_limit`, `session_parked`.
- Identity fields (optional, additive): `user`, `display`, `package`,
  `activity`, `session`, `core`, `backend`/`source`, `id`, `serial`.
- Frame-freshness fields on `shot`: `seq` (host request sequence), `frame`
  (device counter), `frame_ts_ms`, `requested_ms`, `waited_ms`, `fresh` (frame
  answered *this* request), `non_black`, `sha256` (first 16 hex of PNG digest),
  `bytes`, `capture_path`, `row_stride`, `source`. A stale earlier capture can
  never satisfy a later request because only the matching `seq` is accepted.
- UI nodes: `{ref, text, id, desc, cls, bounds, click[, focus, scroll]}`;
  `ref` (`e1`…) valid until the next `ui`.
- Text-tree fields (opt-in): `text`, `text_source` (`uiautomator |
  instrumentation | none`), `text_available`, `text_note`.
- Events: `{seq, type, t, ...}` with `type ∈ {app_started, app_ready, crash,
  anr, log}`; redroid adds `source:"logcat"` and a crash object
  `{type, package, exception, message, stack[], time, tag}`; `--since` is an
  epoch-ms cursor returning `next`.

### 4.3 Contracts worth carrying into ACOS

1. **One-command-one-JSON-object**, additive schema, stable identity on every
   session-touching response.
2. **Request-sequenced frame freshness**: never return a stale screenshot; the
   frame names the request it answered.
3. **Explicit isolation label**: `fork --share-process` requires an explicit
   acknowledgement and returns `isolation:"none"`; `reset` on a shared task
   returns `state_reset:false`.
4. **Uniform error taxonomy with a `hint`**, and `unsupported_backend` /
   `service_unavailable` as first-class failures (never a silent fallback in
   explicit mode).
5. **Park tiers as an explicit dial** (`detach < frozen < app < user`) with
   idempotency and reported `state_kept`.
6. **Signer policy**: `install` reports `signer`; the public AOSP platform key
   is always denied; debug-key install is opt-in (`ACVM_ALLOW_DEBUG_APK`,
   `ACVM_PROFILE=dev`).
7. **Separate reset scopes**: `--app` (fast, data cleared, user/install kept)
   vs. whole-user; the response says `data_cleared` and `regranted`.

### 4.4 Gaps the contract itself documents

No build step; no snapshot/branch (`snap` unsupported on redroid); no network
control / `adb reverse`; no permission/settings wrappers; no push/pull/clipboard/
screen-recording; no scroll-to-element or recents/menu; text tree is opt-in and
one-shot (the 4.1 ms persistent serve variant is research, not shipped).

---

## 5. Benchmark methodology lessons and pitfalls

Directly reusable rules, each with a file pointer.

1. **Write out the memory metric, never mix PSS / workset / `memory.current` /
   host `MemAvailable`.** Always report ≥2 of them; state resolution and settle
   time (`bench/lib/harness.py` `cgroup_mem`/`cgroup_pss_mib`; `EXPERIMENTS.md`
   §18; `plan` in `research/PLAN-task-first.md` §4).
2. **Only compare rows produced in one benchmark session on the same host
   state.** Settled bare anon varied 259–322 MiB across sessions; the fix is
   paired/interleaved A/B with the baseline re-run back-to-back
   (`EXPERIMENTS.md` §24; ITERATIONS "strict rules").
3. **No fixed sleeps in a timed path.** Prove completion with a request-sequenced
   fresh frame (`seq`/`sha256`) and polling. P1.7's `vd_create()` had an
   unconditional `time.sleep(1.5)` that fabricated a "1.52 s display creation"
   and "~2.5 s start" figure later retracted (`EXPERIMENTS.md` §17, §18).
4. **Reject invalid rows.** A boot at the timeout, or no successful UI dump, is
   not a result. Early bare-core readings of 196/69.8 MiB were containers that
   never booted (`EXPERIMENTS.md` §5).
5. **Interleaved A/B (A/B/B/A), ≥3 fresh-core pairs for memory, ≥20 latency
   samples; p50 and p95; 5 % noise floor floored by run-to-run spread.**
   `bench/compare.py` encodes `boot_s`, N=1/N=5 memory, create→shot, shot,
   resume→shot, parked idle CPU, cold→shot (+ extended marginal/reset/clone/
   flake keys). Sequential arms produced false "regressions" resolved only by
   interleaving (v9, v10, W1).
6. **Charge the whole job.** The task-first plan counts build + install + launch
   + observe + reset + fallback + retained core/pool, not a subtracted marginal
   (`research/PLAN-task-first.md` §4). The dev-loop baseline was install-bound:
   `adb install -r` ~19 s = 74–81 % of the loop (STATE/ITERATIONS).
7. **Resolve the baseline artifact, don't sell it.** Bare-baseline drift caused
   fake per-session N=1 deltas in v9/v10/W1; use the product's fresh-core
   `measure_memory` and the image marker wait (`iter/W1.md` §5).
8. **Distinguish cold / warm / restored / parked and separate metrics**
   (guest boot vs host cold path vs create→frame).
9. **Classification gates:** a research renderer preview must never be counted
   as a runtime pass; `accept.sh` e2e is the correctness gate (two sessions,
   differing non-black captures, disjoint trees, typed marker isolation,
   park/resume/reset/destroy, RAM guard, no leak). U1 is the mandatory usability
   gate (14/14).
10. **Safety is part of methodology:** 85 % RAM hard ceiling, spawn lock, no
    scratch in `/tmp`/`/dev/shm`, `ulimit -v 3000000` for all unit tests, one
    unit-test run grew to 32 GB RSS and took the host to 98 % (`EXPERIMENTS.md`
    §6, §20, §21). Never bypass the guard flag (`ACVM_REDROID_SKIP_GUARD`);
    several historical probes do — repair before reuse (`iter/w7_probe.py`,
    `iter/v12_cold_ab.py`, `iter/v12_warm_ab.py`).
11. **Don't trust a single account of KSM/zram/parking.** Compressed memory,
    cgroup charging and host `AnonPages` disagreed (`EXPERIMENTS.md` §11);
    `docker pause` frees 0.
12. **Report unsupported as unsupported.** The R1 host renderer refused
    WebView/GL/service cases (3/3) and produced 0 false passes; W1's media/
    camera/DRM/interactive-permission loss is documented and reversible.

---

## 6. Using the legacy engine later as a behavioural oracle (do NOT run now)

The AOSP track is frozen at `aosp-track-final` (`31da75a`). If and when the user
explicitly authorises using it, these are the documented entry points. **None
were run in this audit, and none should be run without checking host RAM, the
85 % rule, the slot/lock policy, and explicit authorisation.** A CPU-light,
read-only alternative is to use the recorded JSON/`iter/*.md` evidence as a
static oracle.

Product/oracle commands documented by the legacy repo:

```sh
# --- read-only context (safe) ------------------------------------------------
cd /home/Qui/Work/android-container
git status --short && git log -5 --oneline
git tag -l && git branch -a

# --- behavioural comparison (REQUIRES authorisation + slots + RAM guard) -----
# one container at a time, through the slot semaphore, under bench locks
bash tools/slot.sh bash bench/accept.sh --image "$TF_IMAGE" --apk-dir "$TF_APK_DIR"
bash tools/slot.sh bash bench/usability.sh --profile default --image "$TF_IMAGE" \
     --apk-dir "$TF_APK_DIR"
python3 bench/compare.py "$TF_BASELINE_JSON" "$TF_CANDIDATE_JSON"

# --- direct agent-loop oracle on a live core (product CLI) -------------------
export ACVM_BACKEND=redroid
acvm up --count 1 --app com.acvm.p1probe
acvm launch "$ID" com.acvm.p1probe
acvm ui "$ID" --text
acvm shot "$ID" --scale 0.5
acvm tap "$ID" e1 --settle
acvm assert "$ID" --text "NEXT"
acvm reset "$ID" --app
acvm down --all

# --- cold-path / memory A/B drivers (containers; authorisation required) -----
bash tools/slot.sh python3 iter/v12_cold_ab.py --rounds 5   # NOTE: sets SKIP_GUARD; repair first
bash tools/slot.sh python3 iter/w1_mem_ab.py --rounds 3 --settle 60

# --- host-only renderer oracle (no Android; bounded JVM, 3 GB cap) -----------
timeout 600 python3 research/task-first/r1/run.py
```

Constraints if ever used: privileged redroid is **trusted-only** (not an
untrusted-APK boundary); the 85 % RAM rule, spawn lock, 4-slot semaphore,
bench locks, no `/tmp` scratch, no sudo, and no global host changes all apply;
prefer using the frozen engine as an *offline evidence source* rather than
starting containers.

---

## 7. Top findings for ACOS design (summary)

1. The legacy AOSP/redroid track is **frozen** (`aosp-track-final`) and the
   user is already exploring an own-runtime "ACVM Lite" — ACOS is the Rust-first
   continuation of that intent, not a redroid iteration.
2. Per-session memory is dominated by the **app renderer** (SwiftShader/Vulkan
   `[anon:scudo:secondary]`, one ~75 MB mapping per GPU process), not by Android
   user bookkeeping (~0.7 MiB PD/user after v2/v8).
3. The **bare core** floor is 206–312 MiB depending on image and host state and
   is not a proven lower bound; core savings and per-session savings must be
   measured separately.
4. **Frame freshness is a contract, not a nicety** — a request-sequenced
   `seq`/`frame`/`sha256` is the single most reusable idea in the legacy agent
   API.
5. **Reset dominates the dev loop**, and the honest number depends on whether
   new-user dexopt is in the path (`--app` 0.75 s vs whole-user + observe ≈48 s).
6. Boot and cold-path gains saturated: once guest boot < the host adb floor
   (~6 s), further guest cuts do not move the cold column.
7. **Compatibility was never broadly proven**: 21/23 F-Droid apps on display 0,
   no GMS, no committed Compose/Flutter/RN/NDK fixtures.
8. **Privileged containers fail untrusted-APK isolation**; ACOS isolation must
   be a real, audited boundary.
9. **Measurement discipline is the durable asset**: interleaved A/B, memory
   metric identity, no fixed sleeps, invalid-row rejection, whole-job
   GiB-minutes.
10. The **agent contract** (one JSON object, identity fields, error codes,
    isolation labels, park tiers, signer policy, separate reset scopes) is the
    right starting shape for the ACOS CLI/MCP surface.
