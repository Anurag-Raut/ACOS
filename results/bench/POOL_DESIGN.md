# SessionPool — design, isolation analysis and defaults (W25-C / T-44)

Run: the full `tools/bench-acos.sh` harness run
`results/bench/run-2026-11-10-204202.json` (2026-11-10, `release`, `corelib real`
Android-14 boot classpath), 5 fresh child processes per `N`, 10 throughput
rounds per child, workload `fixtures/build/dexprogs/02_int_arith` (`Main`).
This is the companion of `results/bench/MEMORY_PROFILE.md` (W21-B shared boot
DEX) and `results/snapshot/REPORT.md` (W19-E snapshot/restore); the same numbers
are in `results/bench/REPORT.md` and `BENCHMARKS.md` §8.

## 1. What the pool is

`SessionPool` (`crates/acos-runtime/src/session_pool.rs`) is an in-process pool
of `N` worker sessions built from **one** immutable `SessionTemplate`:

1. `SessionTemplate::boot(options)` builds one `Vm` (`Vm::new`) and calls
   `Vm::snapshot()` (T-42/W19-E) while the VM is quiescent. This is the only
   step that pays the boot cost.
2. `SessionPool::start(template, N, limits)` spawns `N` worker threads. Each
   worker **owns its own `Vm`** and creates it *on that thread* with
   `Vm::restore(&snapshot)`.
3. A job is `Vm::run(main)` + (optionally) a reset back to the template state.
   Results are keyed by job id and returned sorted, so the output is
   deterministic for any `N`.

The template is a **core VM**, not a booted application process: the T-42
snapshot preconditions refuse application DEX and non-quiescent state, so the
pool's template is the smallest state that can be restored cheaply. A caller
that needs a pre-booted framework process is outside this pool (that is T-43 /
the zygote-template work).

## 2. What is shared, what is per session

| data | owner | sharing |
|---|---|---|
| boot-classpath raw DEX bytes | `acos_dex::cache::BootCache` | one `Arc<[u8]>` per file, process-wide |
| parsed `DexFile` id tables + string/class index | `BootCache` | one parsed file per fingerprint, process-wide |
| decoded `MethodCode` bodies | `acos_vm::codecache` | one body per `(DexFile::id, code_off)` |
| the template snapshot bytes | `SessionTemplate` | one `Arc<[u8]>`, cloned into each pool |
| application DEX (the job) | the caller | one `Arc<DexFile>` shared by every job |
| class table, statics, handles, interner, heap, stacks, threads, VFS | each `Vm` | **not shared** |

Nothing mutable crosses a `Vm`: no handle, `ClassId`, static slot, monitor or
heap page is shared. Sharing is immutable research data only (DEX text and
decoded code), exactly as W21-B measured for `sessions-shared`.

## 3. Reset = restore, and why it is cheap

`reset(session)` drops the session's `Vm` and restores a fresh one from the
template snapshot. The measured reset wall time (per session, N sessions in
parallel) was **2.41 ms p50 at N=1** and **5.17 ms p50 at N=20**; the copy of
the snapshot bytes is a few KiB and the dominant cost is the warm boot-DEX
re-attach (shared `BootCache`/`codecache` hit, `Vm::new` ≈ 1.3 ms) plus the
snapshot decode. The legacy `reset --app` command was 110.3 ms and reset-to-frame
748.1 ms (`BENCHMARKS.md` §3). **These are not the same metric:** legacy reset is
application-scoped and completes to a fresh frame; `SessionPool::reset` restores
a quiescent core VM with no frame to be ready for. The honest claim is: *core
state restore is ~21–46× cheaper than the legacy app-reset command*, not that the
pool resets an app to a frame in 5 ms.

Reset equivalence is tested by `crates/acos-runtime/tests/session_pool.rs`: a
stateful program (`10_objects_fields`, static mutable `total` + accumulating
`System.out`) run twice in one session leaks state, and after a reset the same
program is byte-identical to the reference run — no leaked statics, no leaked
stdout buffer, no leaked class state.

## 4. Concurrency and failure containment

* **Ownership.** `Vm` is kept on its worker thread for its whole life. Commands
  and jobs travel over `std::sync::mpsc` channels; the pool never sends `&mut Vm`
  or the `Vm` itself across threads. This is required anyway because the
  `acos-runtime` `Runtime` wrapper owns an `Rc<RefCell<ServiceHost>>` (not
  `Send`); the pool deliberately works on `Vm`, not `Runtime`.
* **Panic isolation.** Every job body runs inside `catch_unwind`. A panicking
  session's `Vm` is dropped (it is a local inside the guarded closure) and the
  next command lazily restores a fresh one. The other sessions are untouched.
  Test: `panicking_session_is_isolated_and_replaced` injects one fault, confirms
  the other two sessions complete with identical output, and confirms the
  poisoned worker serves nine more jobs. `catch_unwind` is `UnwindSafe`-wrapped
  with `AssertUnwindSafe` because a half-mutated `Vm` is never inspected: it is
  discarded, not repaired.
* **Watchdog isolation.** Each session gets its own `HostGuard` wall clock and
  guest heap quota. A looping session returns `TIMEOUT` from `Vm::run` (the
  interpreter and allocator call `host.check()`), is classified `Killed`, and is
  restored. Test: `looping_session_is_killed_by_wall_guard` compiles an infinite
  `LoopForever` program and confirms it is killed while the two sibling sessions
  finish and keep working.
* **Determinism.** Results are sorted by `SessionJob::id`; the determinism test
  runs the same six-job list through pools of 1/2/3/5 sessions and requires
  byte-identical `(exit_code, stdout)`.
* **Restore failure** is a reported `JobStatus::RestoreFailed`, counted in
  `PoolStats::restore_failures`; never a silent success.

## 5. Isolation analysis (honest)

**What an untrusted session could do to another one in this process.**

* The shared data is immutable DEX/decoded code, but it is still in the same
  address space. A memory-safety bug or a native escape (the VM crates are
  `#![forbid(unsafe_code)]`, but dependencies such as `rustix`/`libc` are not)
  could read or write another session's heap. Rust's type system prevents
  *accidental* aliasing across `Vm`s, not a deliberate exploit or an FFI bug.
* The process-global `BootCache` and `codecache` are `Mutex`-protected and only
  ever grow by inserting a new entry. A session cannot corrupt another entry,
  but it can force unbounded cache growth (memory DoS) or hold the lock briefly.
* `HostGuard`'s RSS ceiling reads **process** `/proc/self/status:VmRSS`. A
  per-session `rss_max_bytes` therefore stops the whole process, not one session;
  only the wall clock and the guest heap quota are genuinely per session.
* A crash that bypasses `catch_unwind` (abort, stack overflow in a native frame,
  a signal, the OOM killer) takes down the whole pool. `catch_unwind` covers
  Rust panics, not aborts.
* `Runtime::with_host` installs one process-global Binder transport and process
  identity. The pool uses `Vm` directly and does not install it, but any caller
  that wraps pooled `Vm`s in `Runtime` would share one Binder identity/capability
  namespace across sessions — not an isolation boundary.
* There is no cgroup, no seccomp filter, no separate uid/pid namespace per
  session, and no per-session fd table beyond the VM VFS.

**Therefore:** **processes (or zygote-forked children) remain the isolation
boundary for untrusted APKs.** An untrusted tenant must get its own process with
its own transport/capability namespace, cgroup and (ideally) uid. The
`MEMORY_PROFILE.md` recommendation — a warm parent that parses the boot
classpath and then forks isolated children, CoW where the host permits it —
applies unchanged; the `SessionPool` is the in-process half of that design.

**When in-process pooling is acceptable:**

* the agent's own trusted workloads (fixtures, differential runs, benchmarks,
  telemetry, the agent's own generated DEX), where all sessions have the same
  trust level and a bug is a crash to contain, not an adversary to keep out;
* fast reset/restore experiments where the process boundary is provided
  elsewhere (one pool process per tenant, orchestrated by the agent);
* measurement mode — `sessions-shared` and `sessions-pool` exist to measure the
  shared-boot win, not to claim multi-tenant isolation.

## 6. Chosen defaults

| knob | default | rationale |
|---|---|---|
| `SessionLimits::wall_secs` | 60 s | bounds a looping/hung session so it can be replaced; the bench uses 30 s |
| `SessionLimits::heap_max_bytes` | 0 (template value, 256 MiB) | a genuinely per-session guest quota; `0` means "inherit the template" |
| `SessionLimits::rss_max_bytes` | 0 (disabled) | it is a *process* RSS ceiling, not per session; enabling it under `N` sessions stops everyone |
| `SessionJob::reset_after` | `true` | the normal pooled workload is run + reset; `false` is how a caller observes leaked state on purpose |
| pool size | caller-chosen `N` | the bench measures 1/2/5/10/20; CPU efficiency was 0.84 cores/session at N=20 |
| template | quiescent core `Vm` | the T-42 snapshot precondition; boot the app process per tenant instead |

## 7. Measured (real corelib, 5 fresh processes per N, 10 rounds/child)

| N | anon+shmem p50 (MiB) | marginal (MiB/session) | sessions created/s p50 | jobs/s p50 | reset ms p50 | CPU cores p50 | CPU cores/session |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 28.80 | — | 504.3 | 134.5 | 2.41 | 1.17* | 1.17* |
| 2 | 31.37 | 2.56 | 937.0 | 274.1 | 2.06 | 1.46 | 0.73 |
| 5 | 39.06 | 2.56 | 2037.2 | 508.1 | 2.83 | 4.77 | 0.96 |
| 10 | 51.88 | 2.57 | 3023.9 | 733.3 | 3.66 | 8.77 | 0.88 |
| 20 | 77.54 | 2.57 | 3198.7 | 840.4 | 5.17 | 17.01 | 0.85 |

\* N=1 CPU is a tick-granularity artefact: each round is ~7 ms and one host tick
is 10 ms, so 1.17 "cores" is 1 thread plus rounding noise. The efficiency
column is only meaningful for N≥2.

All five `N` produced **one** distinct `(exit_code, stdout)` (`distinct_outputs_max = 1`),
`failed = 0`, `panics = 0`, `timeouts = 0`, `restore_failures = 0`. The ~2.57 MiB
marginal is small because the template is a core VM (no framework app boot) and
the boot DEX/parsed tables/decoded code are shared; compare `sessions-shared`'s
14.3 MiB/session, which additionally runs `boot_app_process` in every session.
The two are different scopes and are reported separately, never mixed.
