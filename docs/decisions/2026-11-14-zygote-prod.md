# Decision 2026-11-14: the production zygote (ZPROD) — persistent, per-session reset, opt-in

Owner decision implemented here: `DECISIONS.md` ADR-0026 decision 1.
Inputs: `docs/decisions/2026-11-12-zygote-shared-art.md`, `results/art-runtime/Z7.md` (the gate
measurement), `results/art-runtime/ZS-security-review.md` (F1–F13), `docs/SECURITY.md`.

Status: **opt-in** (`ACOS_ART_ZYGOTE=on`). It becomes the default only after it wins the Z7 gate on
the real agent loop at N=10 (≥ 25 % whole-system verified tasks/GiB-min against the optimised exec
path, shared boot image included, 0 false passes) — that is P5 below, not this document.

---

## 1. Which zygote: the guest-side ART zygote (Z3/Z4 lineage), not the host template

Two candidate substrates exist today:

| substrate | what a fork actually shares | measured |
|---|---|---|
| host template `ACOS_HOST_ZYGOTE_PLAN` (`crates/acos-art/src/host_zygote.rs`, ZM2) | the Rust *host* prefix: linkerconfig, classpath, guest dex, shims, the empty `ServiceHost` object graph, pre-warmed `keychars` maps | single-digit-% } |
| guest ART zygote `ACOS_ZYGOTE_APP` (`launch.rs` Z3 seam + `research/zygote-proto/{z3,shim}`, Z3–Z7) | ART's whole pre-fork core: boot image mapping, class/intern tables, 34 preloaded framework classes, one CoW heap | **37.8–40.3 % at N=10** (Z7 §3.1) |

The owner's 25 % gate is met by the *shared ART heap*, and only a fork inside the guest process can
share it. A host-side fork cannot hand its pages to a guest that then `exec`s `dalvikvm64`. So:

**The production zygote is the guest-side ART zygote.** The host template stays as the ZM2 research
seam; it is not extended.

## 2. Process tree

```text
acos serve                                  (one daemon per serve; owns keepers)
└── zygote keeper  key = (apk, boot image, geometry, insets, animations)
    │             lives in `crates/acos-art/src/zygote_prod.rs`, driven by the engine
    └── launcher-0  `acos-art launch` — the only process that builds the bwrap sandbox
        └── bwrap            userns + the "zygote pidns" + netns/ipc/uts/cgroup ns, die-with-parent
            └── dalvikvm64 -Xzygote dev.acos.art.ZygoteApp   (PID 1 of the zygote pidns, persistent)
                ├── child tag 0 : z4ns → z4init (PID 1 of *its own* pidns) → app  (LaunchMain.main)
                └── child tag k : same, own mount ns, own run root, own guard
per session, outside the zygote sandbox:
    launcher-k `acos-art launch`, foreign-guest mode (ACOS_ZYGOTE_SESSION=<plan>/<tag>)
               own ServiceHost / InputHost / C1 / logd / sockets / peer; waits for its child
```

Why one launcher per session and not a multi-peer launcher: `ServiceHost`, `InputHost`, C1 and
`logd` are built per launch (`launch.rs`, Z4 §1). Sharing them would be a redesign of the product's
session model, which the 2026-11-12 decision forbids. N launchers cost ~17 MiB Pss each — the same
cost the exec path already pays.

## 3. Ownership and lifetime

* `acos serve` owns at most one keeper per key. A new APK, boot-image mode, geometry, insets mode or
  animations value is a new keeper (a zygote's pre-fork heap is only valid for one such tuple).
* Sessions **attach** to a keeper (`zygote.attach(tag)`); the keeper is refcounted by its live
  sessions and exits after `ACOS_ART_ZYGOTE_IDLE_MS` (default 30 s) with no sessions — a serve must
  not hold a zygote forever.
* The keeper outlives *sessions*, never a *serve*: `serve` shutdown closes every keeper.
* Key material in the keeper directory: `<art-context>/zygote/<key>/{plan/, sessions/<tag>/, log}`.

## 4. Reset = kill one child, fork one fresh child (the Z7 throughput loss)

Z7 measured the prototype's loss (30.0/30.1 tasks/min vs 39.0/39.1) as the cost of restarting the
whole cohort per round — the zygote's pre-fork core is paid every round. The production reset pays
it once per (APK, boot image) and then:

1. `keeper.kill(tag)` → the zygote SIGKILLs that child's namespace leader and reaps it. The child's
   pidns dies with it; **siblings and the zygote are untouched** (this is why the child holds its own
   mount+pid ns, F1/F2).
2. the host deletes `<plan>/<tag>/{run,sock}` → the replacement child gets a **new run root**: empty
   `app-data`, `dalvik-cache`, `out`, `task`, `misc`, `tmp`, and a new socket dir. Nothing the old
   child wrote survives (no state leak).
3. a fresh launcher-k starts in foreign-guest mode and publishes `<plan>/<tag>/{argv,binds,session.json}`
   (unchanged protocol), so the new child gets this session's own binds and its own importer socket.
4. `keeper.spawn(tag)` → the zygote `ZygoteHooks.preFork()`, `fork()`, and the child does the AOSP
   order: `unshare(CLONE_NEWUSER|NEWNS|NEWPID)` → bind its run root → `System.load(production guard)`
   → `postForkChild` → daemons → `LaunchMain.main`. Only the app-specific ~150 ms remains.
5. the reset reply proves freshness exactly like the exec path
   (`crates/acos-cli/src/engine/serve.rs` `reset`): `state_reset:true`, `data_cleared:true`, plus
   the zygote-specific facts `isolation:"fresh-zygote-child"`, a **new `child_pid`**,
   `previous_child_pid`, the new `run_root`, and an **unchanged `zygote_boot_epoch`** (a reset that
   silently restarted the zygote would be visible, not hidden). The bound tree is re-read from the
   new child (`frame_seq` strictly newer).

## 5. The security-review findings this architecture must carry

The owner ruled hostile-APK hardening is not a blocker; host protection stays on every child (same
bwrap argv, same seccomp filter, same Landlock ruleset, same cgroup). F1–F3 are *verification*
correctness, so they are in scope:

| finding | production answer | increment |
|---|---|---|
| **F1** per-child run root / mount ns | child unshares `CLONE_NEWNS` **after** the userns and **before** the seccomp filter, then binds its own `run/{dalvik-cache,misc,tmp,out,task,app-data,app-lib}`, `<tag>/sock` and its importer socket; the filter+Landlock install therefore moves from the zygote's `LD_PRELOAD` constructor to the child (`sb.seccomp_guest(false)` in the foreign path, AOSP's `SpecializeCommon` order). The plan dir is the only shared rw path and holds per-tag subdirs, recreated per reset. | **P1** |
| **F2** per-child pidns | child `unshare(CLONE_NEWPID)` + a fork so the app is PID 2 behind its own init; siblings are invisible, which makes `seccomp.rs`'s "the pidns bounds signalling" sentence true again. Argument-filtering `tgkill`/`rt_sigqueueinfo`/`rt_tgsigqueueinfo` against the child's own tgid is defence in depth on top. | **P1** (pidns) / **P4** (filter) |
| **F3** per-child cgroup + memory limit | every child gets its own scope with `memory.max`, `pids.max`, `memory.swap.max=0`, `memory.oom.group=1`; one session's OOM kills that session only. Protocol: the child blocks on a start-gate byte until the host has moved it into its scope and read back the limits (the commit `confirm()` rule — a stale/absent limit fails the launch, fail-closed). | **P2** |
| **F4** fd hygiene at fork | child closes everything but an explicit allowlist (0/1/2 → its `guest.fifo`, importer socket, `/dev/null`, its own logd socket); each child creates its own logd socket post-fork (`SOCK_CLOEXEC`) instead of inheriting the zygote's cached one. | **P3** |
| **F5** no per-session state pre-fork | the pre-fork phase is a reviewed allowlist and asserts it in the guest; a per-session static written before `preFork()` fails the zygote at boot (today `LaunchMain.sOverlayIdmaps` violates it). | **P3** |
| **F7** `-Xhidden-api-policy:disabled` | stays for now (needed by `Daemons.start()`), listed in `docs/SECURITY.md`; the honest replacement is `ZygoteInit.zygoteInit → RuntimeInit`. | **P6** |
| **F11** `-Ximage` cannot combine with `-Xzygote` | the keeper's key includes the boot image *mode*; with a zygote the image is the stock one, so the H13 `shared-*` image is not used on this path. The ≥25 % gate must therefore be measured against the exec path **with** its `shared-fixed` image (the harder arm). | **P5** |
| **F12** `ACOS_ZYGOTE_APP` is outside the knob table | the production knobs (`ACOS_ART_ZYGOTE*`) get a `docs/SECURITY.md` row with their side effects; the Z3 research seam keeps its own row. | **P1** |

Untrusted APKs stay on the exec path (ZS §0: REJECT for untrusted). The zygote path is for the
trusted fixtures and the owner's agent loop; no claim is made for multi-tenant hostile input.

## 6. Failure handling

* **A dead zygote is restarted and reported, never silently replaced.** Any session bound to it gets
  a structured `ZYGOTE_DEAD` error (`code` in the normal error envelope, with the keeper's exit
  status and last 20 zygote log lines), the keeper starts a new zygote **with a new
  `zygote_boot_epoch`**, and every live session is reforked through the normal reset path (new run
  root, new child pid). The reply carries `zygote_restarted:true` and both epochs.
* A session whose refork fails is left **closed and provably dead** (W55 rule): no bound frame, no
  verb can answer from a stale frame, `liveness()` names the reason.
* `spawn`/`kill` acks are file-based (`<plan>/<tag>/{spawned,killed}`) with a timeout; a timeout is
  the same `ZYGOTE_DEAD` path. Reset is never "best effort".
* Killing a child never touches the zygote: the child is its own pidns leader inside its own cgroup
  (P2); until P2 lands, the child shares launcher-0's scope, so the keeper kills by pid, never by
  cgroup (`cgroup.kill` on that scope is what Z7 §6 measured taking the zygote down).

## 7. The flag

| knob | default | meaning |
|---|---|---|
| `ACOS_ART_ZYGOTE` | unset/`off` | `on` selects the persistent zygote for `acos serve` sessions. Unset = **byte-identical to `main`**: same argv, same calls, same order, same replies. |
| `ACOS_ART_ZYGOTE_BUILD` | built by `research/zygote-proto/z3/build.sh` | the guest dex + shims; a bad dir fails the launch (fail-closed), never falls back to exec |
| `ACOS_ART_ZYGOTE_IDLE_MS` | `30000` | keeper idle exit with no sessions |
| `ACOS_ART_ZYGOTE_N` | `10` | pre-fork budget (how many children one zygote may host before a second keeper is used) |

`ACOS_ZYGOTE_APP` / `ACOS_ZYGOTE_SESSION` / `ACOS_ZYGOTE_PLAN_DIR` stay research seams; the product
knob is the single `ACOS_ART_ZYGOTE` (it sets them internally and refuses to run if the caller set
them to something else).

## 8. Increments and their acceptance

| # | increment | acceptance (all must hold) |
|---|---|---|
| **P1** | persistent zygote; **reset forks one child, siblings untouched**; F1 per-child mount ns/run root; F2 pidns; freshness proof in the reset reply; dead zygote → structured error + restart; flag off unchanged | flag-off gates + Gaten byte-identical to `main` (counter gate, `w22-counter-reset`, `w54-dialog-pixels`, `w42-back-frame`, `w15-dialog-menu`, `form-validation`, `suite/T03-render-good`, `v1-offscreen-pager` pass; `T03-render-wrong`, `negative/offscreen-pager-visible`, `p11-progress-timeout` fail with their codes; Gaten 5/5 flows + 5/5 negatives). Flag-on: counter gate and one Gaten flow pass. Tests: (i) two sessions, reset A → A's `child_pid` changes, A's counter returns to `0`, **B's pid and counter are unchanged**, zygote `boot_epoch` unchanged; (ii) killed zygote → `ZYGOTE_DEAD` + new epoch + both sessions reforked fresh; (iii) flag off → the engine spawns exactly the same argv/calls as `main` (argv assertion test). |
| **P2** | F3: per-child cgroup with `memory.max`/`pids.max`/`oom.group`, start-gate handshake, fail-closed `confirm()` | from inside a child: `memory.max` equals the requested value; N=2, one child allocates past its limit → **that child dies, its sibling and the zygote stay alive and verified** (`memory.events` per scope); a missing limit fails the launch (`CGROUP_UNAVAILABLE`), never runs unbounded |
| **P3** | F4 fd allowlist + per-child logd socket + F5 pre-fork invariant assert | child `/proc/self/fd` is exactly the allowlist; a log frame forged in A never appears in B's `logs` reply; a per-session static written pre-fork aborts the zygote boot with a named error |
| **P4** | F2 defence-in-depth: `tgkill`/`rt_sigqueueinfo`/`rt_tgsigqueueinfo` argument filter built post-fork | in-child `tgkill(sibling)` → `EACCES`; the filter is installed per child with its own tgid (unit test on the filter builder + a probe in a N=2 run) |
| **P5** | **the owner gate**: default-on decision | Z7 `loop.py` at N=10, ≥3 interleaved reps, keeper included in the ΣPss denominator, **≥25 % verified tasks/GiB-min vs the optimised exec path *with* `shared-fixed`**, tasks/min not lower than the exec path, 0 false passes, load1 + MemAvailable recorded per cell. Only then does the default flip, and the flip is its own commit with the numbers in `EXPERIMENTS.md`. |
| **P6** | F7 hidden-API seam, idle/keepalive policy, zygote memory accounting in the leaderboard | `-Xhidden-api-policy:disabled` gone with the Gaten flows still green; `ACOS_ART_ZYGOTE_IDLE_MS` honoured (keeper exits, /proc empty); the zygote's own PSS is in the reported denominator |

P1 is this task. P2–P6 are filed in `results/handoff/ZPROD1.md` with the exact commands.
