# ZS — security review of the Zygote-track sandbox (gate input for the owner rule)

**Read-only review.** No crate, guest, shim or fixture was edited; ART was not run. Only this
file was written. Method: source reading with `file:line`, plus the evidence already committed by
Z1/Z2/Z3/B2/B3/S1/S5/S6/S7. Every claim is marked **VERIFIED** (I read the code or the committed
evidence) or **INFERRED** (reasoned from code/evidence, not executed). Where a claim would need a
run, §8 states the test instead.

Owner rule under test: *"Keep untrusted APKs on the existing execution path until a new sandbox
passes security review."* This document is that review's input, not the decision.

---

## 0. Verdicts (details in §4–§6)

| workload class | exec+bwrap path (today) | Z2 in-process zygote sandbox | Z3 zygote child (as built) | verdict for the zygote path |
|---|---|---|---|---|
| **Trusted first-party test APKs** | certified, in use | acceptable **research** sandbox: no new *dangerous* hole at N=1, four gaps to close before it is a product sandbox | acceptable: the child carries **exactly the production kernel layers** (same bwrap args, same seccomp filter, same Landlock, same cgroup) | **PASS with conditions** — fixes F4, F5, F6, F7, F9 (§7). Not for multi-session use until N>1 is evidenced (F1–F3). |
| **Untrusted third-party APKs** | CERTIFIED-WITH-LIMITS (`docs/SANDBOX_CERTIFICATION.md` §1, §8.7) | **FAIL** | **FAIL** | **REJECT. Keep on the exec path.** No uid boundary (F8), shared address layout (F10), shared pidns/cgroup at N>1 (F1–F3), no N>1 evidence at all, inherited fds and pre-fork statics (F4, F5). |

The single most important finding, and the reason the two rows differ:

> **At N=1 a Zygote child is not less confined than an exec'd guest — it is the *same*
> confinement. Every zygote-specific security loss appears only at N>1** (cross-session `/proc`,
> signalling, cgroup DoS, shared run-root files, layout sharing among *siblings*), **and
> today's evidence stops at N=1** because the launcher accepts exactly one guest connection
> (`Z3.md` §6.1; one `accept(listener)` at `crates/acos-art/src/launch.rs:421`). So the zygote path has
> been *measured* precisely in the regime where it cannot be distinguished from the safe path,
> and *unmeasured* in the regime that decides the question. A security verdict for N>1 is
> therefore not merely unfavourable — it is **unsupported by evidence**, which for an
> untrusted-input gate counts as a fail.

Two structural facts that shape everything below:

1. **Z3's child inherits the product's whole kernel sandbox.** `launch.rs` builds one bwrap argv
   (`SANDBOX_FLAGS`, `crates/acos-art/src/lib.rs:880-893`: `--unshare-pid --unshare-net
   --unshare-ipc --unshare-uts --unshare-cgroup-try --die-with-parent --clearenv --cap-drop ALL`),
   installs the guest seccomp filter (`seccomp.rs:140-205`, installed by the `LD_PRELOAD`
   constructor `seccomp_guest.c:403-431`) and the Landlock ruleset (`seccomp_guest.c:298-398`,
   plan from `landlock.rs:105-129`), and runs the tree in one cgroup scope
   (`launch.rs:746-748`, `cgroup.rs:142-193`). A forked child inherits **all** of it. Z3 adds no
   new kernel boundary; it removes per-*session* independence.
2. **Z2's sandbox is a second, weaker policy bolted on afterwards, and it cannot be composed with
   fact 1.** Z2 installs its own userns/mountns/pidns and a 40-entry *blocklist*
   (`research/zygote-proto/shim/z2fork.c:315-356`). The product filter already denies `mount`
   and `unshare` (`seccomp.rs:153,157`), so a child forked from a product zygote **cannot**
   create Z2's mount namespace at all. Z2 works today only because it is not combined with Z3
   (`Z3.md` §6.5 says exactly that). This is the central composition defect (F1).

---

## 1. Memory-layout secrecy

| | (a) exec+bwrap | (b) Z2/Z3 child |
|---|---|---|
| boot image base | random per session with the stock LZ4 image; **fixed `0x70000000` when `ACOS_SHARED_BOOT_IMAGE` is on** — `launch.rs:163`, `:598-599` (`-Ximage … -Xnorelocate`), verified in `B3.md` §3 ("mapped at exactly `0x70000000-0x70468000` in all five sessions") | **inherently fixed and shared**: the child inherits the parent's mapping, so every child of a zygote agrees on the image base whether or not B3 is on. Z1 §6 measured the image group at 44 KiB private dirty in every child, i.e. one physical, identically-mapped copy. VERIFIED (Z1 §6, Z2 §4) |
| heap / class table / intern table addresses | independent per process | **identical in the parent and every child**. Z1 §0: `System.identityHashCode(String.class)` = `142058083` in all children; `inherited=42` identical, `private=1008…5056` differ. VERIFIED |
| can child A learn child B's addresses? | Not from layout (they differ); only by a bug that leaks across processes, which the pidns/netns/uid boundary makes hard | **Yes, given any memory disclosure in A** (a native read past a buffer, an uninitialised C1 pixel buffer, a `ByteBuffer` over-read): the addresses it learns for the pre-fork region are valid in B *and* in the zygote. That is the classic zygote ASLR-sharing property. INFERRED (no disclosure primitive was constructed — correctly, out of scope) |
| post-fork randomness | n/a | the child's own post-fork allocations (region space, stacks, JIT code cache) are still randomised by the kernel at `mmap`/`brk` time, so the sharing is limited to the pre-fork region. VERIFIED from the mapping table in Z1 §3 (`region space live bitmap` private per child) |

**Winner: (a) exec.** And note the two arms compound: **B3's fixed image base buys nothing new in
the direction of secrecy once a zygote exists** — the zygote already fixes that base for all
children. B3's security note (`B3.md` §5, "trusted workloads only … an open owner decision") should
be read together with this section: adopting the zygote makes B3's ASLR cost strictly redundant,
so if the zygote is rejected for untrusted work, B3 should be too, for the same reason, and vice
versa — the owner should decide them as one ASLR policy, not two.

**What Android does about it** (INFERRED as a *design* statement; the individual mechanisms are
VERIFIED in `.cache/aosp`):

* AOSP does **not** re-randomise ASLR per child: it accepts that all children of a zygote share
  one layout, and puts the barrier *between* processes instead of between layouts. Per child it
  calls `setresuid(uid)` (`base/core/jni/com_android_internal_os_Zygote.cpp:2022`), then
  `selinux_android_setcontext(uid, …)` (`:2134`), after isolating the app's data dirs by tmpfs
  overmount in a private mount namespace (`:1937-1950`, `ensureInAppMountNamespace` `:854`,
  zygote-side `unshare(CLONE_NEWNS)` `:2344`). Those three — distinct uid, distinct SELinux
  domain, private data view — are what make "A knows B's addresses" useless.
* The pre-fork region is deliberately kept free of secrets: ART's `PreZygoteFork`
  (`art/runtime/gc/heap.cc:2438-2553`) trims and freezes the heap and moves the class table so the
  shared part is *data that every app is allowed to see* (boot image, preloaded framework classes).
  `ZygoteHooks` are used here for exactly that (`Z1.md` §1).
* Android's own response to the sharing literature has been structural, not ASLR: one zygote per
  ABI, a separate app-zygote for isolated processes, and never a shared Java process across apps
  (`docs/decisions/2026-11-12-zygote-shared-art.md` "Never put independent apps inside one shared
  Java process" is the same rule).

ACOS has **none** of the three barriers (§3), which is why layout sharing matters far more here
than on Android.

---

## 2. Filesystem isolation

| | (a) exec+bwrap | (b) Z3 child | (c) Z2 child |
|---|---|---|---|
| root view | `--tmpfs /` + explicit binds; **per-run `/data`**: `system app overlay lib shim` + props read-only, and only `dalvik-cache misc tmp out task app-data` bound read-write from `run/` (`lib.rs:1041-1095`) | **inherited verbatim** at N=1 (same run root, same binds) — VERIFIED `launch.rs:661` (`sb.run_root(run.path())` runs before the fork decision) | the product root **plus** two tmpfs overmounts (`/data/data`, `/data/zygote-private`) and a fresh `/proc` (`z2fork.c:464-475`); no Landlock |
| per-session data | one run root per launch | **a resident zygote fixes `/data` at zygote start**; every child then shares that one run root unless a per-child bind is made — and a per-child bind needs `mount`, which the product filter denies (`seccomp.rs:153`) → **F1** | `/data/data/pkg-<tag>` is private per child (Z2 §3.1: sibling `ENOENT`, `iso_data_listing=[pkg-n]`), but `out/ task/ app-data/` stay shared, i.e. one child can read/overwrite another's frames and trees |
| Landlock | mandatory, fail-closed (`landlock.rs:105-145`; `seccomp_guest.c:298-398`) | inherited ✔ | **absent** — the mount hiding is the only path boundary (Z2 §6.3 says so) |
| zygote's private dir | n/a | n/a | over-mounted with empty tmpfs (Z2 §3.1 `iso_zygote … ENOENT`) ✔ |
| `/proc` | fresh procfs in the per-launch pidns, so only the guest's own tree | fresh procfs in the **shared** zygote pidns: at N>1 every child sees every sibling's `/proc/<pid>/{environ,cmdline,fd}` and, with same uid + dumpable, **can open `/proc/<B>/fd/<n>`** and inherit B's socket. `ptrace_scope=1` (read: `/proc/sys/kernel/yama/ptrace_scope` = 1) blocks `/proc/<pid>/mem` but **not** `PTRACE_MODE_READ` paths. INFERRED from proc(5)/Yama semantics; the test is in §8 | fresh procfs in the child's own pidns ✔ (Z2 §3.1: `pid=[4026533825/…]` distinct per child) |
| fd inheritance | one process, own fds | **the zygote's fds are inherited by every child**: the `logd` `SOCK_DGRAM` fd cached in a static (`shim/artshim.c:158-176`, `log_fd`, no `SOCK_CLOEXEC`), plus the zygote's own stdio. `Z3.md` §1.3 records this as known. VERIFIED | same (Z2 is forked from the same process), plus A/B helper fds; `pipe2(O_CLOEXEC)` used (`z2fork.c:432`) ✔ |

**Winner: (a) exec** for a multi-session product, and **tie at N=1** between (a) and (b): a single
child of a zygote sees exactly the exec path's view. Z2 is strictly weaker than (a) at N=1
(no Landlock, shared `out/`), and it is not composable with (a) because of the `mount`/`unshare`
denial (F1).

---

## 3. Identity

| | (a) exec+bwrap | (b) Z2 | (c) Z3 |
|---|---|---|---|
| uid | the caller's uid (bwrap's implicit userns; `SANDBOX_FLAGS` deliberately has no `--unshare-user`, `lib.rs:881-882`), `--cap-drop ALL` | **uid 0 inside a one-uid userns** = the same outer uid (`z2fork.c:427`, Z2 §2 table: `setresuid(10000) → -EINVAL`, `setresuid(0,0,0)` succeeds) | the caller's uid, inherited |
| caps | bwrap: implicit userns zeroes them; `--cap-drop ALL` on top (`lib.rs:891-892`) | `PR_CAPBSET_DROP 0..40` + `capset` zero (`z2fork.c:389-398`) after the mounts, before app code | inherited from the guest (none) |
| SELinux / LSM identity | none | none | none |

Nobody in this repo has a distinct uid — that is a property of the whole unprivileged design, not
of the zygote. But the *consequence* changes with the topology, and this is the crux of the
identity question:

In a zygote all sessions are the **same uid in the same pidns and the same cgroup**. On Android
that combination is safe only because the uid is *not* shared: `setresuid(uid)` per child
(`com_android_internal_os_Zygote.cpp:2022`) plus a per-app SELinux domain (`:2134`) means app A's
`kill`, `ptrace`, `/proc/<B>/*`, `/proc/<B>/fd/*`, and any world-readable-but-app-private file are
all denied by DAC/LSM. Strip the uid split and **only the seccomp filter and the mount view remain**
— and the product filter explicitly delegates the signalling part of that job to the pid
namespace (`seccomp.rs:47-53`: "the guest pid namespace (`--unshare-pid`) is what bounds
signalling in the first place"). A shared zygote pidns voids that sentence.

**Winner: (a) exec**, not because its uid is better (it is the same uid) but because a per-launch
pidns is what stands in for the missing uid, and a per-launch exec keeps it. Concretely, what a
distinct uid would block and a shared-uid zygote does not:

* signalling a sibling session (F2) — `kill`/`tkill` are denied (`seccomp.rs:196-197`) but
  **`tgkill` and `rt_sigqueueinfo`/`rt_tgsigqueueinfo` are not in the deny list at all**
  (`seccomp.rs:140-205`; the reason is documented at `:47-53`) → cross-session signal DoS at N>1.
* opening a sibling's `/proc/<pid>/fd/<n>` (→ its host-peer socket, its logd socket) (§2).
* reading a sibling's `/proc/<pid>/environ` and `cmdline` (session ids, paths).
* any future per-session file that is not in the mount view but is DAC-readable to the uid.

---

## 4. Syscall filter

| syscall | production ART filter | Z2 blocklist | AOSP app policy |
|---|---|---|---|
| shape | **deny list**, 60 entries, `EACCES` (`seccomp.rs:140-205`), + `AF_UNIX`-only `socket`/`socketpair` (`:378-385`), + `clone` namespace-flag mask (`:411-415`), + `clone3 → ENOSYS` (`:398-401`), + `personality` limited to `PER_LINUX`/query (`:403-408`), + wrong-arch ⇒ `KILL_PROCESS` (`:371-374`) | **deny list**, 40 entries, `EPERM` (`z2fork.c:315-356`), + wrong-arch ⇒ `KILL_PROCESS` (`:361-363`); **no argument filtering of any kind** | **allow list**, per-arch `app_policy.cpp`, installed per child by `set_app_seccomp_filter()` from `SetUpSeccompFilter` (`com_android_internal_os_Zygote.cpp:646-660`; the allowlist header is `#include <seccomp_policy.h>` at `:83`) — the policy files themselves are **not** in the sparse checkout, so their content is INFERRED |
| `ptrace`, `process_vm_*` | denied ✔ | denied ✔ | denied |
| `io_uring_setup/enter/register` | denied ✔ (`seccomp.rs:168-170`) | **allowed** ✘ (absent from `z2_blocked`) | denied on kernels that have it (INFERRED) |
| `bpf` | denied ✔ | denied ✔ | denied |
| `userfaultfd` | denied ✔ | denied ✔ | denied |
| `perf_event_open` | denied ✔ | denied ✔ | denied |
| `keyctl`/`add_key`/`request_key` | denied ✔ | denied ✔ | denied |
| `pidfd_open`/`pidfd_getfd`/`pidfd_send_signal` | denied ✔ (`:193-195`) | **allowed** ✘ | denied (INFERRED) |
| `kcmp`, `process_madvise`, `mount_setattr`, `fsopen/fsconfig/fsmount/fspick/move_mount/open_tree` | denied ✔ | **allowed** ✘ | denied |
| `personality` | narrowed to `PER_LINUX` ✔ | **allowed** ✘ (e.g. `ADDR_NO_RANDOMIZE`, `READ_IMPLIES_EXEC`) | denied |
| `clone3` | `ENOSYS` so bionic falls back to the filtered `clone` ✔ | **allowed, unfiltered** ✘ → namespace creation and `CLONE_VM`-less forks bypass the `fork`/`vfork`/`unshare` denials | denied |
| `fork`/`vfork` | allowed (needed by nothing, but not blocked) | denied ✔ | allowed? (INFERRED; apps may `Runtime.exec`) |
| **`execve`/`execveat`** | **allowed, deliberately** (`seccomp.rs:142-147`: "not a host escape … the new image inherits the filter, the mount view, the cgroup and the namespaces") | **allowed** (absent from the blocklist) — and Z2 §6.6 flags it | allowed (INFERRED) |
| `kill`/`tkill` | denied ✔ | **allowed** ✘ — only the pidns bounds it | allowed (bounded by uid) |
| `tgkill`/`rt_sigqueueinfo`/`rt_tgsigqueueinfo` | **allowed, unfiltered** ✘ (`:47-53` documents that the *pidns* is the barrier) | allowed ✘ | allowed (bounded by uid) |
| `socket(AF_INET|AF_NETLINK|AF_PACKET…)` | denied (AF_UNIX only) ✔ | **allowed** ✘ (only the bwrap `--unshare-net` hides the network) | allowed for some domains (INFERRED) |

**Winner: (a) exec** on shape (deny list is still not AOSP's allowlist, but it covers every
syscall the brief names except `personality`-adjacent details and signalling), and the gap to AOSP
is a *policy* gap shared by both arms. Z2 loses the direct comparison outright: no io_uring, no
pidfd, no clone3 limit, no argument filters, `personality` open, signalling open.

The important *new* finding is not in either list, it is in the composition:

> The production filter's own documentation justifies allowing `tgkill`/`rt_*sigqueueinfo`
> because "the guest pid namespace is what bounds signalling" (`seccomp.rs:47-53`). A zygote
> topology in which several sessions share one bwrap pidns **invalidates that justification
> without changing a single line of the filter.** Any zygote adoption must therefore be
> accompanied by an argument filter on those three syscalls (the logic already exists in
> `crates/acos-sandbox/src/linux/seccomp.rs:384-420`, but it needs the child's pid at
> filter-build time, which is only knowable *after* the fork — i.e. the filter must be installed
> per child, not by the zygote's `LD_PRELOAD` constructor). This is F2 and it is coupled to F1.

---

## 5. Inherited state from the zygote

| item | state in the Z3 zygote at fork time | risk |
|---|---|---|
| heap contents | boot image + preloaded framework + `LaunchMain.applySystemOverlays`/`registerNatives` results, `gcAndFinalize`d (`ZygoteApp.java:47-62`); 61–68 MiB `Shared_Dirty` in every child (Z1 §6, Z3 §4) | **low today**: no app data, no credential, no per-session nonce is created pre-fork — VERIFIED by reading the pre-fork list (`Z3.md` §1.1) and by `ZygoteApp.java:35-62`. The risk is *structural*: the invariant "nothing session-specific before `preFork()`" is enforced by review only, and `-Xzygote` freezes whatever is there. Android keeps the same invariant by construction (the zygote never runs app code) |
| class/intern table | `MoveClassTableToPreZygote`/`InternTable::AddNewTable` (`Z1.md` §1, `heap.cc:2438-2553`) | intended; per-child entries are private (Z1 §6) ✔ |
| **per-session static written pre-fork** | `LaunchMain.sOverlayIdmaps = args[7]` (`research/zygote-proto/z3/java/dev/acos/art/ZygoteApp.java:42`, static at `LaunchMain.java:76`) | **F5**: a per-session value is written into the shared pre-fork heap. Harmless for one child; for a resident zygote the value of the last `preFork` is inherited by the next child, and if a future seam forgets to reset a static the *previous session's* identifier/path leaks into the next app. Today: leak of the overlay path list only |
| fds | `logd` socket, cached statically, no `SOCK_CLOEXEC` (`shim/artshim.c:158-176`) | **F4**: every child shares one logd socket → (i) a child can forge log frames that the host attributes to *the session it is reading*, destroying log-evidence per session; (ii) the fd survives `execve` (allowed) into any program the child execs. Verified `Z3.md` §1.3 + code |
| signal dispositions | ART's own (`SigIgn=0000002000000007`, `SigCgt=…` — Z2 §1.1); `SIGCHLD` reset to `SIG_DFL` by the shim (`z2fork.c:168`) | **F6**: app code inherits the parent's ignore/catch mask rather than a clean one (`Z2.md` §6.8). Low severity, but it is inherited *state*, and a zygote is the one place where "reset to init state" is cheap |
| env / rlimits | `--clearenv` + explicit `--setenv` (`lib.rs:1197-1230`) ✔; `RLIMIT_CORE=0`, `RLIMIT_NOFILE=4096` in `pre_exec` (`launch.rs:765-770`) ✔ | inherited by all children; no core dump, no fd explosion. `PR_SET_PDEATHSIG` (`launch.rs:755`) is **cleared in the child by `fork`** — but the zygote is PID 1 of the bwrap pidns, so its death kills the whole namespace regardless. INFERRED (pidns semantics), and it is a *good* property |
| hidden-API policy | `-Xhidden-api-policy:disabled` is on the zygote argv (`launch.rs:644-645`) and the child calls `Daemons.start()` reflectively (`Z3.md` §2) | **F7**: a product that ships this hands every app the hidden-API surface *and* removes ART's own guard, for the sake of one reflective call. The honest replacement is AOSP's own path (`ZygoteInit.zygoteInit → RuntimeInit`, `Z3.md` §6.3) |

**Winner: (a) exec** — a fresh process inherits nothing but its own argv and the product's fixed
environment. (b) inherits a 60 MiB writable-shared heap, a live socket, ART's signal mask and a
build-time hidden-API switch.

---

## 6. Cross-session attacks and denial of service

| attack | (a) exec | (b) Z2 | (c) Z3 |
|---|---|---|---|
| A signals B | impossible: separate pidns per launch | pidns per child (`z2fork.c:433-473`) ⇒ invisible; but `kill` **is** allowed inside the child, and A/B helper stages share the zygote pidns | **possible at N>1**: shared pidns + `tgkill`/`rt_*sigqueueinfo` allowed (F2). `kill`/`tkill` still denied |
| A ptrace/`process_vm_readv` B | pidns + seccomp | denied ✔ + pidns | denied by filter, invisible by pidns? no — same pidns, filter only |
| A reads `/proc/<B>/*` or B's sockets | impossible (pidns) | impossible (pidns) | **possible at N>1** for same-uid `PTRACE_MODE_READ` paths, incl. `/proc/<B>/fd/<n>` → B's host peer socket (F3) |
| A reaches B's files | separate run root | `out/`, `task/`, `app-data/` shared; `data/data` private | shared run root at N>1 (F1) |
| A attacks **the zygote** | n/a | A/B stages run in the zygote's pidns *before* the sandbox exists (the window Z2 §6.7 names); C cannot (own pidns) | the zygote is PID 1 of the bwrap pidns ⇒ immune to signals without a handler and teardown-safe; but a compromised native constructor running pre-sandbox (Z2 §6.7) executes in the zygote's own address space only if it is the zygote's own code — app code is post-fork, so this is not reachable. Residual: `ptrace` from a child is denied, `/proc/<zygote>/mem` needs `PTRACE_MODE_ATTACH` (Yama 1) ⇒ denied. **This axis is acceptable** |
| fork bomb | `pids.max=512` per launch (`cgroup.rs:48`) | blocklist stops `fork`/`vfork` but **not `clone3`** (allowed) nor `clone` without `CLONE_VM` ⇒ **unbounded fork bomb inside the child** (bounded only by the shared cgroup) | bounded by `pids.max`, but the budget is **shared with the zygote and every other session** ⇒ **cross-session DoS** (F3) |
| memory | `memory.max` per launch (default 2 GiB, `cgroup.rs:44`), `memory.swap.max=0`, `memory.oom.group=1`, fail-closed | same cgroup, shared; **no per-child limit** | `oom.group=1` means one session's OOM **kills the whole cgroup** = all sessions + the zygote (F3) |
| CPU | no per-run CPU cap (host-wide) | same | same, + one session's CPU competes with the zygote's forks |

**Winner: (a) exec**, decisively. This is where the zygote model costs the most: three of its four
privilege separations (pidns, cgroup, mount view, uid) are *per-launch* today and a zygote makes
them per-*tree*.

---

## 7. Required fixes before untrusted APKs could use a zygote path

Effort: S ≤ 1 h, M ≈ 1 day, L ≥ several days (same scale as S1 §5). "AOSP equivalent" is the
mechanism that shows the shape is achievable, not something ACOS can copy wholesale (ACOS has no
root, no SELinux policy, no `installd`).

| # | Fix | Where | AOSP equivalent | Effort | Blocks |
|---|---|---|---|---|---|
| **F1** | **Per-child mount namespace and per-session `/data`, created *before* the seccomp filter.** Move the filter install from the zygote's `LD_PRELOAD` constructor (`seccomp_guest.c:403`) to a per-child step after its mounts, in AOSP's order; then each child gets `out/ task/ app-data/ socket/` bound from its own run root. | `lib.rs:1041-1095`, `seccomp.rs:enable`, `seccomp_guest.c:403`, `launch.rs:661` | `SpecializeCommon` mount-data step `:1937-1950` → `SetUpSeccompFilter` `:2017` → `setresuid` `:2022`; zygote-side `unshare(CLONE_NEWNS)` `:2344` | **L** | untrusted, multi-session |
| **F2** | **Argument-filter `tgkill`/`rt_sigqueueinfo`/`rt_tgsigqueueinfo`** against the child's own tgid, or keep a per-child pidns. (The filter needs the pid, so this lands with F1.) | `seccomp.rs:140-205`, port `acos-sandbox/src/linux/seccomp.rs:384-420` | per-app uid ⇒ `kill` DAC (no filter needed) `:2022`, + SELinux `:2134` | **M** | untrusted, N>1 |
| **F3** | **Per-child pidns and per-child cgroup scope** (`memory.max`, `pids.max`, `oom.group` per session, not per tree); never let one session's OOM kill the group. | `launch.rs:746-748`, `cgroup.rs:142-193`; Z2's A/B/C reaper (`z2fork.c:409-505`) is the cost model (3.6 MiB marginal PSS/child, Z2 §4) | per-app cgroup via `libprocessgroup`/`ProcessList`; `lmkd` | **M** | untrusted, N>1 |
| **F4** | **FD hygiene at fork**: close everything except an explicit allowlist; give each app child its own logd socket (or a `dup` with a per-session tag); add `SOCK_CLOEXEC`. | `shim/artshim.c:158-176`, child entry `ZygoteApp.java:66` | `__android_log_close()` + `AStatsSocket_close()` in `SpecializeCommon` `:2127-2128`; `close()` of inherited fds | **S** | trusted product use |
| **F5** | **No per-session state before `preFork()`**: make the pre-fork phase a reviewed allowlist (currently `LaunchMain.sOverlayIdmaps = args[7]` violates it, `ZygoteApp.java:42`) and assert it in the guest. | `ZygoteApp.java:35-62` | `ZygoteInit.preload` + `ZygoteConnection` split (no app data pre-fork) | **S** | trusted product use |
| **F6** | **Reset signal dispositions / masked signals to a clean state in the child** (AOSP's `SpecializeCommon` does not, but AOSP apps never share a signal mask with the zygote's own handlers because the zygote installs none that matter). | child entry after `postForkChild` | n/a (AOSP relies on `sigaction` defaults + `RuntimeInit`) | **S** | untrusted |
| **F7** | **Delete `-Xhidden-api-policy:disabled`**: replace `Daemons.start()` with the real post-fork entry (`ZygoteInit.zygoteInit → RuntimeInit`) or curate a single documented seam. | `launch.rs:644-645`, `ZygoteApp.java` | `ZygoteInit.zygoteInit`/`RuntimeInit.applicationInit` | **M** | untrusted |
| **F8** | **An identity boundary.** Unprivileged `setresuid` to a distinct uid is impossible (`Z2` §2, `-EINVAL`; probe at `z2fork.c:485-486`). Either (a) accept uid-sharing explicitly in the threat model and compensate with F1+F2+F3+F4 (the only unprivileged option), or (b) delegated subuids + a per-child userns mapped to a distinct host uid (`newuidmap`), which is a deployment requirement, not a code change. | `z2fork.c:427`, docs | `setresuid(uid)` `:2022` + `selinux_android_setcontext` `:2134` | **L** (b) / **S** (a, docs) | untrusted |
| **F9** | **Replace the Z2 blocklist with AOSP's allowlist** (or with the product's deny list + argument filters), and decide `execve` explicitly; validate with the S6 audit mode (`ACOS_ART_SECCOMP_AUDIT=kill`). | `z2fork.c:315-356`; `seccomp.rs:140-205` | `set_app_seccomp_filter` `:646-660`, `app_policy.cpp` | **L** | untrusted |
| **F10** | **Write the ASLR-sharing risk down and decide it**: all children share the pre-fork layout, and B3's fixed image base is subsumed by it. No unprivileged fix exists (AOSP does not have one either — it has uid+SELinux instead). | `B3.md` §5, this §1 | none | **S** (decision) | untrusted |
| **F11** | **`-Ximage` cannot be combined with `-Xzygote` today**: `-Xzygote` sets `only_use_system_oat_files_`, so a `/data/img` image aborts with `Registering a non /system oat file` (Z1 §5.2, measured). Provision the image under `/system` or an APEX-like path before the two arms can be evaluated together. | `launch.rs:163,598-599`, `run.sh`/sysroot provisioning | image lives in `/system/framework/*/boot.art` | **S** | any combined evaluation |
| **F12** | **Register `ACOS_ZYGOTE_APP` in `docs/SECURITY.md`'s knob table and remove its side effects**: the seam binds an arbitrary host directory read-only into the guest (`launch.rs:684-685`) and `fs::copy`s a `.so` into the *shared* `<data>/shim` rather than a per-run root (`launch.rs:683`). A research seam that copies from an env-named directory into the shared data root violates the per-run-root rule (S4) and is invisible to the knob audit. | `launch.rs:584-591, 676-686`, `docs/SECURITY.md` | n/a (Android has no such seam) | **S** | any use, trusted included |
| **F13** | **Evidence at N>1.** No isolation claim about a zygote is meaningful until 2+ real app children exist (blocked today by the one-guest peer, `launch.rs:421`). Repeat Z2 §3's probes (sibling `/proc`, `/proc/<B>/fd`, `tgkill`, fork bomb, OOM) with N=2 before any gate. | launcher + `crates/acos-art/tests` | — | **M** | the gate itself |

**Top 5 (the order I would fund):** F1, F2+F3 (one packet), F13, F9, F4. F7/F8 are policy decisions
that belong with them; F10/F11/F12 are cheap and can land immediately.

---

## 8. Claims I did not test (and the test that would settle each)

1. **`/proc/<B>/fd/<n>` reachability between zygote siblings.** Test: two children of one zygote in
   the *product's* bwrap pidns; in A, `open("/proc/<B>/fd/<n>")` for every n of B, and try
   `write()` on the result (expect: succeeds for the peer/logd sockets, `EACCES` after F1+F3).
2. **Cross-session signalling.** Test: A calls `syscall(SYS_tgkill, pid_B, tid_B, SIGTERM)` and
   `rt_sigqueueinfo(pid_B, SIGTERM, …)` inside the product filter (expect: `-EACCES` for `kill`,
   but success for `tgkill`/`rt_sigqueueinfo` today — proves F2).
3. **Fork bomb via `clone3`.** Test: a child issues `clone3` in a loop (expect: `ENOSYS` under the
   product filter, i.e. safe; under Z2's blocklist expect: succeeds until `pids.max`).
4. **OOM group kill.** Test: one session allocates past `memory.max` in a 2-session zygote; read
   `memory.events`/`oom_kill` of the shared scope and count surviving sessions (expect: all die —
   proves F3).
5. **logd forgery.** Test: A writes a frame to the inherited `logdw` fd during B's session and check
   how the host attributes it (`logd.rs:141-175`).
6. **F1 in practice.** Test: after moving the seccomp install to the child, confirm the child can
   still `unshare(CLONE_NEWNS)` + `mount` and that the app's own `mount` attempt afterwards is
   `EACCES`.
7. **`-Ximage` + `-Xzygote`.** Test: `ACOS_ZYGOTE_APP=… ACOS_SHARED_BOOT_IMAGE=.cache/b2/image`,
   expect the Z1 §5.2 CHECK failure (F11).

---

## 9. Files read (for the next reviewer)

`results/art-runtime/{Z1,Z2,Z3,B2,B3,S1}.md`; `docs/{AOSP_REFERENCE,SECURITY,SANDBOX_CERTIFICATION}.md`;
`docs/decisions/2026-11-12-zygote-shared-art.md`; `crates/acos-art/src/{launch,lib,seccomp,landlock,cgroup,logd}.rs`;
`crates/acos-art/shim/artshim.c`; `crates/acos-art/src/seccomp_guest.c`;
`crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`; `research/zygote-proto/shim/z2fork.c`;
`research/zygote-proto/z3/java/dev/acos/art/ZygoteApp.java`;
`crates/acos-sandbox/src/linux/seccomp.rs` (for the argument-filter logic the ART filter lacks);
`.cache/aosp/base/core/jni/com_android_internal_os_Zygote.cpp` (`SpecializeCommon` `:1898`,
seccomp `:646-660`/`:2017`, `setresuid` `:2022`, SELinux `:2134`, mount ns `:854`/`:1937-1950`/`:2344`;
`art/runtime/gc/heap.cc` citations from `Z1.md` — the `art/runtime` sparse checkout is empty in
this tree, so ART internals are taken from Z1's committed reading).
