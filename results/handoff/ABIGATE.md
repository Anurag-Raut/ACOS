# ABIGATE — the ART path refuses a wrong-ABI APK *before* the guest

**Worktree:** `/home/Qui/.paseo/worktrees/3oxa6xao/k5`
**Branch:** `abi-gate` (from `rel3-int` = `19db6b8`, the v0.2.0 release candidate).
Not pushed, not merged. **Track:** ABIGATE (release blocker, small).

---

## 1. Problem this fixes

`docs/AGENT_GUIDE.md` (Limits) and `docs/ERROR_CODES.md` promise that an APK whose
native code has no `lib/x86_64/` entry is `UNSUPPORTED_ABI` on every route — "a
gate, not a silent fallback". On the ART path it was not:

* `crates/acos-services/src/native_libs.rs` decided `NativeLibInstall::UnsupportedAbi`
  correctly, and `crates/acos-art/src/launch.rs` **logged it and started the guest
  anyway** ("the guest must still start and fail with its own structured
  `UnsatisfiedLinkError`"). The guest then died before its first frame, so the
  caller saw `GUEST_EXITED` (CLI/MCP envelope) or, on the older path, `INTERNAL`
  "confined child closed stdout". Neither names the real cause, and an agent could
  read a harness error where the documented answer was a capability gate.
* REL3a recorded this as its one unmet goal (`results/handoff/REL3a.md` §3,
  "Goal 4 (ABI) — NOT met, recorded honestly"), with the exact next action:
  "an `acos-art` error variant + CLI classification that returns
  `ErrorCode::UnsupportedAbi`".

## 2. What changed

### 2.1 The decision is AOSP's, in `acos-apk` (`crates/acos-apk/src/apk.rs`)

New, with the citation in the doc comment:

* `SUPPORTED_ABIS: [&str; 1] = [HOST_ABI]` — this host's `Build.SUPPORTED_ABIS`.
* `enum AbiGate { NoNative, Supported { abi }, Unsupported { apk_abis, libs_sample } }`
  and `Apk::abi_gate()` / `Apk::native_libs_sample(max)`.

The rule is **per package**, exactly as `PackageManager` does it
(`frameworks/base/core/jni/com_android_internal_content_NativeLibraryHelper.cpp`):
`findSupportedAbi` keeps the `lib/<abi>/` directory whose index in the device's
supported list is lowest, returns `NO_NATIVE_LIBRARIES` when the APK has no `lib/`,
and `INSTALL_FAILED_NO_MATCHING_ABIS` when it has `lib/` but no match;
`iterateOverNativeFiles` then extracts only the chosen ABI's directory.

### 2.2 The refusal happens before the guest (`crates/acos-art/src/launch.rs`)

At the `native-libs` stage — before props, logd, the peer socket, the image or any
guest process — `LaunchResult` for an `AbiGate::Unsupported` package is:

* `outcome: Unsupported`, `code: "UNSUPPORTED_ABI"`, `application_created=false`,
  `activity_launched=false`, `guest_exit=null`, `wall_ms=0` (`abi_gate_result`);
* a new `LaunchResult.abi` field (`{apk_abis, host_abi, libs_sample}`), serialized
  into the result JSON **and** into the session's own `session-exit.json`;
* on the interactive protocol, an `ok:false, cmd:"ready"` line carrying the code
  and the same three fields — the same shape `HOST_DISK_FULL` already uses, so a
  caller waiting for a ready line never mistakes a refusal for a dead session.

Handling `NativeLibInstall::UnsupportedAbi` is now the only place the launcher can
see that variant, and it returns immediately.

### 2.3 Classification (`crates/acos-cli/src/failure.rs`)

* `map_art_code("UNSUPPORTED_ABI") => ErrorCode::UnsupportedAbi` — a registered
  code, so it can never be reported as `INTERNAL`/`GUEST_EXITED`.
* `session_exit_facts` keeps `abi`, and `attach_abi_facts` lifts `apk_abis`,
  `host_abi`, `libs_sample` to the **top** of `error.details` (in both `art_error`
  and `attach_session_exit`, i.e. the `launch`/`serve` and `verify` paths).

No MCP change was needed: session tools forward the daemon envelope verbatim
(`crates/acos-mcp/src/daemon.rs`), so `acos_launch` carries the code and details.

## 3. Acceptance evidence

All runs used the worktree's own build and scratch, the exclusive ART lock and the
guarded runner (`tools/art-runtime/run-exclusive.sh --task ABIGATE -- …`).
CLI/MCP runs use the scratch install
`WT/.cache/abigate/prefix` (`tools/release/install.sh --prefix`, real release
build of `acos`, `acos-art`, `acos-mcp`).

**Test APK** `WT/.cache/abigate/hello-jni-arm64-only.apk`: `fixtures/build/apps/hello-jni.apk`
repacked with every `lib/` entry dropped and the `lib/x86_64/libhellojni.so`
payload moved to `lib/arm64-v8a/libhellojni.so`, then `zipalign -f 4` +
`apksigner` (debug keystore, v1+v2+v3), `apksigner verify` clean. It ships exactly
one ABI directory and no `lib/x86_64/`.

| route | result |
| --- | --- |
| `acos launch <arm64-only.apk> --engine art --json` (installed prefix) | exit 1, `ok:false`, `status:"unsupported"`, `error.code:"UNSUPPORTED_ABI"`, `retryable:false`, `details.apk_abis:["arm64-v8a"]`, `details.host_abi:"x86_64"`, `details.libs_sample:["lib/arm64-v8a/libhellojni.so"]` |
| `acos verify scenarios/v2-art-counter.yaml --apk <arm64-only.apk> --engine art` | exit 1, `status:"unsupported"`, `UNSUPPORTED_ABI` with the same details (no verdict, no steps ran — never a pass) |
| MCP `acos_launch` (installed `acos-mcp`, empty cwd outside the checkout, private ART daemon) | `ok:false`, `status:"unsupported"`, `UNSUPPORTED_ABI`, `retryable:false`, `details.engine:"art"`, `apk_abis`/`host_abi`/`libs_sample` as above (10/10 checks in `.cache/abigate/mcp_abi.jsonl`) |
| counter (pure Java, no `lib/`) `acos launch --engine art` | `ok:true`, `status:"pass"`, `stage_reached:"first_frame"`, `engine:"art"` |
| gaten (x86_64 native libs) `acos launch --engine art` | `ok:true`, `status:"pass"`, `stage_reached:"first_frame"` |
| gate `scenarios/v2-art-counter.yaml` | pass, `art-reference`, 11 steps |
| gate `scenarios/w22-counter-reset.yaml` | pass, `art-reference`, 9 steps |
| gate `scenarios/gaten-home.yaml` | pass, `art-reference`, 14 steps |

Unit tests (new):

* `crates/acos-apk/tests/native_extract.rs`: `abi_gate_refuses_an_arm64_only_apk_with_its_inventory`,
  `abi_gate_passes_a_pure_java_apk`, `abi_gate_passes_an_apk_with_both_abis_and_picks_the_host_one`,
  `abi_gate_passes_a_mixed_apk_per_package_like_aosp`.
* `crates/acos-services/tests/native_libs_install.rs`: the arm64-only case now also
  asserts the `sample` inventory.
* `crates/acos-art/src/launch.rs` (`launch::abi_gate_tests`): the refusal's shape
  (`outcome != Ok`, `code == UNSUPPORTED_ABI`, no guest, `abi` block present and
  serialized) and that a guest-route record carries no `abi` block.
* `crates/acos-cli/src/failure.rs`: `UNSUPPORTED_ABI` in the explicit mapping table,
  plus `the_abi_gate_refusal_carries_its_inventory_and_is_not_a_guest_death`
  (`details.apk_abis/host_abi/libs_sample` at top level, not retryable, and
  `classify_session_exit` agrees).

`cargo test -p acos-apk` green; `cargo test -p acos-art -p acos-cli -p acos-mcp`
green (logs: `WT/.cache/abigate/test-*.log`).

### Host contamination found during acceptance (worth knowing)

A `acos serve` from the **previous** `~/.local` install (pid 2716687, started
23:12:20Z by another track with `PWD=/home/Qui/Projects/gaten`) was listening on the
default runtime socket `/run/user/1000/acos/session.sock`. `acos launch --engine art`
(`run_keep → ensure_daemon → try_daemon`) and `acos-mcp` (whose `discover_binary()`
found `~/.local/bin/acos` first on `PATH`) both forwarded to that **old** binary and
reproduced the pre-fix `GUEST_EXITED`. That daemon was not killed (not ours). The
acceptance runs isolate from it honestly and say so: `ACOS_SESSION_SOCKET` points at
a scratch socket, and MCP's `PATH` starts with the installed prefix's `bin` so its
private daemon is the prefix's own code. Nothing was masked: the same call against
the old daemon is exactly the bug this track fixes.

## 4. Decisions

1. **Gate placement: before the guest, in `acos-art`, not in the CLI.** The CLI and
   the MCP daemon both spawn the same `acos-art launch`, so one refusal covers
   `acos launch/verify --engine art`, `acos serve` sessions and MCP
   `acos_launch`/`acos_verify`. A CLI-only gate would leave `acos serve` (the MCP
   route) unguarded.
2. **`UNSUPPORTED_ABI` is a package-level gate; the mixed-ABI case follows AOSP.**
   An APK with `lib/x86_64/liba.so` **and** an arm64-only `libb.so` **launches**:
   `findSupportedAbi` picks x86_64 (the only ABI in this host's supported list that
   the APK provides), `iterateOverNativeFiles` extracts only `lib/x86_64/*`, and the
   first `dlopen("libb.so")` fails with the app's own `UnsatisfiedLinkError` — a
   packaging bug the platform reproduces, not an install gate. Reporting it as
   `UNSUPPORTED_ABI` would be a *false* refusal (the APK does ship host code), so
   the rule is the AOSP one and the mixed case is a test, not a special case.
   Consequences: pure-Java APKs always launch (no ABI question), both-ABI APKs
   launch as x86_64, arm64-only APKs are refused.
3. **Refusal shape copies `HOST_DISK_FULL`.** A pre-guest refusal that writes no
   ready line looks like a dead session to an interactive caller, which is how the
   old path produced `INTERNAL`. The ready line + `session-exit.json` + a
   `LaunchResult` with `outcome: Unsupported` keeps every consumer on the same code
   path (`status: "unsupported"`, `retryable: false`, never a pass).
4. **The details contract is `apk_abis`/`host_abi`/`libs_sample`** at the top of
   `error.details` (documented in `docs/ERROR_CODES.md`), mirrored inside
   `details.session_exit.abi` for callers that read the session record. `libs_sample`
   is capped at 8 entries, ABI-then-name sorted, so a huge APK cannot bloat an
   envelope.
5. **No MCP-side mapping.** The daemon envelope is forwarded verbatim; adding an
   MCP table would create a second place to forget the code.

## 5. Remains

* Nothing outstanding for this track. The mixed-ABI `dlopen` failure has no test
  that reaches the guest (it would need a mixed-ABI fixture plus a JNI call); the
  classification (pass-through, no gate) is covered by unit tests.
* The foreign `~/.local` daemon on the default socket is still running; a
  fleet-wide `acos serve` version check / socket ownership handshake would have
  caught it. Out of scope here.
* RN/Flutter routes were already gated (`acos-runtime`, `acos-jni`) and are
  unchanged.

## 6. Cleanup

Deleted at the end of the track: the scratch install
`WT/.cache/abigate/prefix` (1.7 GB), the artifact trees of the CLI/verify/gate
runs, the repacked intermediate APKs and the scratch sockets. `WT/.cache/abigate/`
is now 248 KB (the signed arm64-only test APK, the MCP check log, the gate
envelopes and the test logs). No session daemon, guest or bubblewrap of ours is
left running — `pgrep -af 'acos serve'` shows only two **other tracks'** daemons
(the foreign `~/.local` one and a `wt-view1` bench daemon). `~/.local` was not
touched and no MCP registration was changed. Disk peaked at 85 % (`/`, 757 G of
945 G used) and is 85 % after cleanup; host RAM stayed around 52 % of 60 GiB
(MemAvailable ≥ 18 GiB), with the guarded build capped at 14 GiB and the test
runs at 10 GiB.

---

## 7. Addendum — release-profile rerun and one co-tenant flake (for the record)

The scratch install and the acceptance runs above were done with the debug
profile. After a host-disk prune deleted `WT/.cache/k5-target/debug` the suites
were rerun in the **release** profile (same code, rebuilt):

* `cargo test --release -p acos-apk -p acos-services`: all green
  (`abi_gate_*`, `an_arm64_only_apk_is_structured_unsupported_abi` with the new
  `sample` inventory).
* `cargo test --release --no-fail-fast -p acos-art -p acos-cli -p acos-mcp`:
  **439 passed, 0 failed, 5 ignored** (`WT/.cache/abigate/tests-release2.log`).
* One intermediate run failed `acos-art --test launch_hang`
  (`guest_exits_before_milestone_returns_exit_code_and_cleans_up`,
  "launch took 5.436856176s … bound: 5 s") while three other worktrees were
  running the same release suite (load average 5-8). Re-run alone under the same
  lock it passes in **1.61 s** (bound 5 s). That test times a guest-exit report on
  an already-ended guest and is untouched by this track (no code in that path
  changed); the isolated pass plus the full green rerun is the evidence, and it is
  recorded here rather than hidden.

Compact evidence committed under `results/abigate/`: the CLI `launch`/`verify`
envelopes for the arm64-only APK, the counter/gaten launch envelopes, the MCP
check lines, and the three gate verdict summaries.
