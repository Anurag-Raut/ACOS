# GLSHIP — software GL ships ON by default: the unused cost is zero, the wall is the window renderer

Branch `glship` from `glspike4` (`ffb485af`), 2026-11-18. Private
`ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET` per run (`.cache/gsr-<arm>`),
`ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`, every ART run through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh` (shared slots; the
timings below are *indicative*, not a bench-owner A/B), release binaries in
`.cache/glship-target`, fixtures built with `tools/build-apps.sh`.

## 0. Answer

Software GL (ANGLE on SwiftShader, `OpenGL ES 3.1`) is now the **default** for
every launch: when `<sysroot>/gles/angle-swiftshader` is installed, the launcher
binds its `lib64` read-only at `/vendor/lib64` and serves
`ro.hardware.egl=angle` + `ro.hardware.vulkan=pastel`. `ACOS_GUEST_GLES=off`
turns it off, `=<tag>` picks another staged provider (`mesa-host` is reserved
for the GPU track), and a host without the provider runs **exactly today's
guest** (`gles: none`, no bind, no property, no digest).

The cost of an app that never touches EGL is **zero within measurement**:
launch step p50 154 ms with the provider vs 154 ms with it off (5 reps each),
per-session peak RSS 13.9 vs 14.0 MiB, M8 fast action→state 246 vs 243 ms
(counter) and 205 vs 196 ms (form) — all inside the per-rep spread. The digest
of the 32 MB provider is paid **once per host** (`<provider>/.verified`,
89 ms) and **0 ms** on every later launch.

One wall stays, and it is not GL: a HARDWARE bitmap drawn into an ordinary
`View` still dies in the framework's software window canvas
(`Software rendering doesn't support hardware bitmaps` → `LOOPER_THREW`), as on
a device with software rendering. GPU **readbacks** work and are asserted
(`gles-software` 3/3, `a11y-screenshot-wrap`).

## 1. What changed

| area | change |
|---|---|
| `crates/acos-art/src/gles.rs` | `DEFAULT_PROVIDER=angle-swiftshader`, `OFF=off`, `RESERVED_MESA_HOST=mesa-host`; one selection rule `status()` (default-on-if-installed / `off` / explicit-must-exist) used by the bind, the property table, `acos doctor` and `acos_capabilities`; `verify()` now caches the digest per **stat fingerprint** in `<provider>/.verified`; `verify_full()` is the always-hash path; `capability()` for the reports |
| `crates/acos-art/src/lib.rs` | the sandbox bind uses `status()`; the trace line names default vs flag |
| `crates/acos-art/src/props.rs` | `for_launch()` resolves through the same `status()`; the property test covers the 4-way matrix (not installed / installed / `off` / named) |
| `crates/acos-art/src/gles.rs` tests | +6: default-on, `off` (case-insensitive), explicit miss is an error, digest cached until the fingerprint moves (measured), touched file re-hashes, same size+mtime is the documented accepted risk |
| `crates/acos-cli/src/doctor.rs` | new `gles` check: `gles: angle-swiftshader \| none` + the reason (`ok` when a provider is selected, `warn` — not `fail` — when not) |
| `crates/acos-mcp/src/backend.rs`, `Cargo.toml` | `acos_capabilities` gains `"gles": {"provider", "detail"}` from the same rule (one implementation: it calls `acos_art::gles::capability()`) |
| `tools/art-gles.sh` | the manifest carries a `provenance` block (ICD sha256 + ELF build id, EGL source, upstream URLs, per-upstream licence, staging time); the EGL source records the input the caller named |
| `tools/art-sysroot.sh` | **new step 5c**: the provider is staged as part of the sysroot build (`--no-gles`, `--gles-icd PATH`, `ACOS_GLES_ICD`); idempotent; without an ICD it stages nothing and says so; `READY` records `gles_provider=` |
| `tools/release/install.sh` | reports the shipped provider (`provider \| version \| licence \| MiB`) in `MANIFEST.txt` and the summary, fails closed on an unreadable manifest; the sysroot copy already carried it |
| `scenarios/a11y-screenshot-wrap.yaml`, `scenarios/gles-software.yaml` | assert the new default (GL path) while keeping the explicit `skipped=no_software_gles` state legal on a host without the provider; comments no longer claim the image has no GL |
| `docs/AGENT_GUIDE.md` | limits: software GL available, GL ES 3.1, the window-canvas wall stated plainly, the cost, the flags |

## 2. Default selection (one rule, four cases)

`gles::status(root)` is the only place that decides:

| `ACOS_GUEST_GLES` | provider installed | result |
|---|---|---|
| unset/empty | yes | `angle-swiftshader` — **default** (`detail: "default; installed at …"`) |
| unset/empty | no | `none` (`"angle-swiftshader is not installed at …"`), today's guest |
| `off` (any case) | either | `none` (`"ACOS_GUEST_GLES=off"`) |
| `<name>` | yes | `<name>` (`"ACOS_GUEST_GLES=<name>"`) |
| `<name>` | no | **structured error**, never a silent fall back to the default |

`mesa-host` is `RESERVED_MESA_HOST`: an ordinary explicit name, so it is only
ever used when someone stages it (the GPU track does); nothing selects it
implicitly.

Reports (measured on this host):

```
$ acos doctor
[ok  ] gles               angle-swiftshader (default; installed at <sysroot>/gles/angle-swiftshader)
$ acos_capabilities   ->  "gles": {"provider": "angle-swiftshader", "detail": "default; installed at …"}
$ ACOS_GUEST_GLES=off acos doctor
[warn] gles               none (ACOS_GUEST_GLES=off)
```

A missing provider is a `warn`, not a `fail`: every app still launches, only
GPU readbacks are `UNSUPPORTED_BACKEND`.

## 3. The unused cost is zero (measured)

`results/handoff/GLSHIP/run-ab.sh` — 5 interleaved reps per arm, one private
runtime dir per rep, `scenarios/counter-fast.yaml` and
`results/handoff/GLSHIP/scenarios/form-fast.yaml` (the gate scenarios with the
fast settle on the input steps, so the tap/type `duration_ms` is the M8 fast
action→state latency). Full table: `ab/summary.md`; raw per rep:

| app | arm | M8 fast per rep (ms) | wall per rep (ms) | peak RSS per rep (MiB) |
|---|---|---|---|---|
| counter | default ON | 240, 248, 247, 239, 246 | 3070, 3082, 3120, 3106, 3086 | 14, 13, 13, 13, 14 |
| counter | `off` | 238, 239, 243, 257, 265 | 3068, 3066, 3100, 3120, 3108 | 14, 14, 13, 13, 13 |
| form | default ON | 205, 197, 709*, 218, 199 | 2418, 2410, 2979*, 2399, 2431 | 13, 13, 13, 14, 13 |
| form | `off` | 196, 194, 722*, 209, 195 | 2441, 2428, 2971*, 2435, 2441 | 13, 13, 13, 13, 14 |

`*` one stalled rep in **each** arm (shared slots, another agent's build), which
is why the p50 is reported next to the spread: the arm-to-arm delta is smaller
than the rep-to-rep spread in every row.

| metric (p50) | counter ON vs off | form ON vs off |
|---|---|---|
| launch step (the step that pays bind + props + stat walk) | 154 vs 154 ms (**Δ 0.0**) | 155 vs 155 ms (**Δ 0.0**) |
| total verify wall | 3086 vs 3100 ms (−0.5 %) | 2418 vs 2441 ms (−0.9 %) |
| M8 fast action→state | 246 vs 243 ms (+1.2 %) | 205 vs 196 ms (+4.6 %) |
| per-session peak RSS | 13.9 vs 14.0 MiB (−0.7 %) | 13.8 vs 13.9 MiB (−0.7 %) |

**Digest cost, per host** (`ACOS_GUEST_GLES_TRACE=1`, `run-arm.sh`,
`verify-first/`, `verify-second/`):

```
first launch after a stage (no .verified):  verify 89ms, staged 65038816 bytes
every later launch (cache hit):             verify  0ms, staged 65038816 bytes
```

So: 89 ms once per staged inode set, 0 ms per launch after that, and the launch
step itself is unchanged at the 1 ms resolution the verdict records. Nothing is
mapped until libhwui asks libEGL for a context (GLSPIKE §5, re-confirmed here by
the RSS equality on `counter`, which never touches EGL).

## 4. Gates at the default flag (no `ACOS_GUEST_GLES` in the environment)

`results/handoff/GLSHIP/run-accept.sh` → `run-gates.sh`, one lock acquisition:

| run | verdict | wall |
|---|---|---|
| `gate-counter` (`scenarios/counter.yaml`) | **pass** | 6107 ms |
| `gate-form` (`scenarios/form-validation.yaml`) | **pass** | 7077 ms |
| `gate-a11y-probe` (`scenarios/a11y-probe.yaml`) | **pass** | 4533 ms |
| `gate-a11y-wrap` (`scenarios/a11y-screenshot-wrap.yaml`) | **pass** | 946 ms |
| `gate-a11y-wrap-off` (same scenario, `ACOS_GUEST_GLES=off`) | **pass** | 943 ms |
| `gate-webview-probe` (`scenarios/webview-probe.yaml`) | **pass** | 455 ms |
| `gate-gles-1/2/3` (`scenarios/gles-software.yaml`, 3 concurrent) | **pass 3/3** | 950 / 950 / 950 ms |

The GL arm really ran (guest logcat, `gate-gles-1`):

```
gles_draw start=1 provider=angle
gles_draw kind=hwbuffer_screenshot ok=true src=1080x2400 config=HARDWARE dst=64x64 mean=248 samples 16,16=#FFF8F8FA …
gles_png_hardware ok=true config=HARDWARE width=64 height=64
gles_draw kind=png_hardware ok=true src=64x64 config=HARDWARE dst=64x64 mean=85 samples 16,16=#FFFF0000 48,16=#FF0000FF …
gles_view skipped=software_window_cannot_draw_hardware_bitmap (opt-in acos.probe.gles_view=1)
```

and with the provider off (`gate-a11y-wrap-off`) the guest is today's guest:

```
gles_draw skipped=no_software_gles
```

## 5. The digest cache: per host, next to the provider

Owner decision (2026-11-18): verify once per host, re-hash only when a file's
`(size, mtime, inode)` changes, keep the fingerprint next to the provider.

* Path: `<sysroot>/gles/<tag>/.verified` — one per provider **tag**
  (`angle-swiftshader` now, `mesa-host` when the GPU track stages it), shared by
  every run on the host, copied with the sysroot into an install. Only
  `lib64/` is bound into the guest, so the guest never sees it.
* Fingerprint: `sha256(manifest-as-the-caller-holds-it)` + each file's
  `(dev, ino, size, mtime_ns)`. A re-stage (new inode), a touched file (new
  mtime) or a different manifest re-hashes; `chmod` does not (it changes only
  `ctime`, which is deliberately not part of the fingerprint).
* Fail closed unchanged: a missing/unreadable/size-mismatched file is the same
  structured `Error::Layout` as before — with or without a cache entry, because
  the fingerprint is computed from `stat` and the full path re-checks size +
  sha256.
* **Accepted risk, documented in the module docs and pinned by a test**: bytes
  rewritten **in place with the same size and the same mtime** on the same inode
  are trusted. `tools/art-gles.sh` publishes the provider atomically (new
  directory, new inodes), so nothing in ACOS can produce that state; the
  alternative is hashing 32 MB on every launch for a case the tooling cannot
  create.
* Tests: `the_digest_is_cached_until_the_fingerprint_changes` (cache hit proven
  by a `chmod 000` file that still verifies, and *measured*: cache hit
  `< 1/10` of a full 8 MiB digest), `a_touched_file_is_hashed_again`,
  `same_size_and_mtime_is_the_documented_accepted_risk`.

## 6. Shipping it (image + install)

* **Sysroot build** (`tools/art-sysroot.sh`, step 5c): stages
  `<sysroot>/gles/angle-swiftshader` with `tools/art-gles.sh` from the *same*
  SDK system image the sysroot extraction already reads (`vendor.img`, ANGLE)
  plus the SwiftShader ICD (`--gles-icd` / `$ACOS_GLES_ICD`, else a local copy).
  No ICD → nothing staged, one log line, guest unchanged. `READY` records
  `gles_provider=angle-swiftshader`.
* **Install** (`tools/release/install.sh`): the sysroot copy already carried the
  provider; the installer now reports it (`provider | version | licence | MiB`)
  and fails closed if the manifest is unreadable. Nothing new to download.
* **Provenance** (`manifest.json`, written by `tools/art-gles.sh`):

  | field | value |
  |---|---|
  | `tag` / `vulkan_tag` | `angle` / `pastel` |
  | `provider` | ANGLE (Vulkan backend) on SwiftShader's CPU Vulkan ICD (`pastel`, Cuttlefish `guest_swiftshader`) |
  | `version` | ANGLE 2.1.24303 (API-34 SDK system image `vendor.img`, 2024-07-12) + SwiftShader ICD build `880ce0810bf6e473eb62b1b6eeea0f6c` |
  | `licence` | `BSD-3-Clause (ANGLE), Apache-2.0 (SwiftShader)` |
  | `provenance.egl_source` | `system-images/android-34/google_apis/x86_64/vendor.img (lib64/egl/angle)` |
  | `provenance.icd_source` | `/home/Qui/.cache/acvm/mem-r1/img/vulkan.pastel.so` — **local** image (acvm `mem-r1`, the AOSP-14 redroid userspace), not a pinned download |
  | `provenance.icd_sha256` | `7af4f7c905c72b28405e6deebc06e78ce692b6bc79f086f445d26b01c7cef3b9` (23 380 960 B) |
  | `provenance.icd_elf_build_id` | `880ce0810bf6e473eb62b1b6eeea0f6c` |
  | staged files | 8 names, 4 inodes, 65 038 816 B on disk (32 MiB, hardlinked) |

  **Provenance gap, stated honestly** (unchanged from GLSPIKE2 §4.1): the
  official AOSP Cuttlefish artifact URL + build id for `vulkan.pastel.so` is
  **not pinned**; the bytes come from the local acvm image. The manifest now
  carries the ICD's own sha256 and ELF build id, so any candidate download can
  be checked byte-for-byte before it replaces this one. Smallest next action:
  fetch a Cuttlefish `aosp_cf_x86_64_phone` guest image, `debugfs`-dump
  `/vendor/lib64/hw/vulkan.pastel.so`, compare the two digests; if they match,
  record the build id in the manifest and the docs.

## 7. Tests

```
cargo test --release -p acos-art -p acos-cli -p acos-services
  acos-art lib:      204 passed, 0 failed, 1 ignored   (198 at glspike4 → +6)
  all targets:       118 test binaries, 0 failed        (.cache/test-full2.log)
```

New/changed tests: the default-on matrix (default / `off` / explicit-miss), the
fingerprint cache (hit measured, touched re-hash, same-size+mtime accepted
risk), the props table 4-way matrix, and `doctor`'s `gles` check surface.

## 8. Repro

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
tools/build-apps.sh
tools/art-runtime/run-exclusive.sh --task GLSHIP -- tools/guarded.sh --mem 14G --time 1500 -- \
    results/handoff/GLSHIP/run-accept.sh      # gates + a11y-wrap-off + the 5-rep A/B
tools/art-runtime/run-exclusive.sh --task GLSHIP -- tools/guarded.sh --mem 10G --time 400 -- \
    bash -c 'rm -f .cache/art/sysroot/gles/angle-swiftshader/.verified; \
             results/handoff/GLSHIP/run-arm.sh verify-first  scenarios/counter.yaml ACOS_GUEST_GLES_TRACE=1; \
             results/handoff/GLSHIP/run-arm.sh verify-second scenarios/counter.yaml ACOS_GUEST_GLES_TRACE=1'
```

## 9. Evidence index

`run-accept.sh`, `run-gates.sh`, `run-arm.sh`, `run-ab.sh`, `summarize-ab.py`,
`scenarios/{counter,form}-fast.yaml` · `gate-*` (verdicts + logs + guest
logcat) · `gate-a11y-wrap-off` (the explicit-off arm) · `ab-counter-*`,
`ab-form-*`, `ab/summary.md` (the cost A/B) · `verify-first/`,
`verify-second/` (the 89 ms → 0 ms digest measurement). Each run dir keeps
`verdict.json`, `steps/`, `logs/logcat.txt`, `launcher.stderr.log`,
`resources.json`; the `frames/`, `tree/` and `art-frames/` dumps were pruned
after the fact (20 MB of PNGs; `verdict.json` names what they held), so a
`frames/NN.png` reference in a verdict is a path this directory no longer
carries.

## 10. Environment / cleanup

Host RAM peak ≈ 24 GiB of 60 (3 concurrent sessions + build); disk 77–83 % of
`/` (stop line 85 %). Deleted at the end: `.cache/glship-target`,
`.cache/gsr-*`, `.cache/lv`, `.cache/glship-probe`, `.cache/glspike-scratch`,
`.cache/patch_*.py`, the `fixtures/build` copy. Kept: the re-staged shared
provider `<sysroot>/gles/angle-swiftshader/` (with `.verified` and the new
provenance manifest) and this directory.
