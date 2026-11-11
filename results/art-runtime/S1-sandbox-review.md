# S1 — Sandbox review of the ART execution route for untrusted APKs

Status: read-only review. No crate was edited, ART was not run, nothing was committed.
Only this file was written. Method: source reading with `file:line` references, plus one
read of `/proc/sys/kernel/yama/ptrace_scope` (value `1`). Items marked **(verify)** are
inferred from kernel or bwrap behaviour and have not been tested here.

Files read: `crates/acos-art/src/{lib.rs,launcher.rs,app.rs,session.rs,launch.rs,logd.rs,main.rs}`,
`crates/acos-art/Cargo.toml`, `crates/acos-sandbox/src/{lib.rs,confine.rs,linux/mod.rs,linux/landlock.rs,linux/seccomp.rs}`,
`tools/guarded.sh`, `docs/ART_RUNTIME_PLAN.md` §4–5, `docs/OPERATING_DIRECTIVES.md` §5,
`docs/SECURITY.md`, `results/art-runtime/F.md` §4.

## Verdict

**The ART route is not safe for third-party APKs today.** `bwrap` is used as a filesystem-view
sandbox only. It shares the host PID, network, IPC and UTS namespaces, inherits the whole host
environment, applies no seccomp filter, no Landlock ruleset and no memory cap, and gives the guest
a read-write `/data` that the host re-reads on later runs.

Two facts shape everything below:

1. **`acos-art` does not depend on `acos-sandbox`.** `crates/acos-art/Cargo.toml:17-23` lists
   only `acos-binder-peer`, `acos-services`, `libc`, `serde`, `serde_json` and `thiserror`.
   The Landlock+seccomp+`no_new_privs` design used by the Rust-VM/JNI/arm64 routes is not in
   this path. `docs/ART_RUNTIME_PLAN.md:230-233` asks for bwrap namespaces *then* Landlock/seccomp.
   That step was never implemented.
2. **Nothing in `acos-art` enforces memory.** The only memory-related knob is `RLIMIT_AS`, which
   is off by default and unusable for ART (`launcher.rs:64-68`). `ACOS_HOST_RSS_MAX` is enforced
   only by the Rust VM (`acos-vm` HostGuard). `acos-art` merely mentions it in an error string
   (`launcher.rs:414,445`).

---

## 1. Isolation: bwrap route vs Landlock+seccomp route

Source of the bwrap argv: `Sandbox::command`, `crates/acos-art/src/lib.rs:657-773`.
Source of the other route: `crates/acos-sandbox/src/confine.rs:295-395` and `linux/*.rs`.

| Layer | bwrap route (`acos-art`) | Landlock+seccomp route (`acos-sandbox::confine`) |
|---|---|---|
| user namespace | Not requested (no `--unshare-user`). bwrap uses an unprivileged userns, so the guest runs as the caller's non-root uid. **(verify)** the effective caps are empty after exec. | Optional (`NamespacePlan`, default off, `confine.rs:124-138`). |
| mount namespace | Always (bwrap). Root is `--tmpfs /` (`lib.rs:661-662`), so `/home`, `/tmp`, `/run` and the repo are hidden unless bound. | Optional; `MS_REC|MS_PRIVATE` (`linux/mod.rs:252-272`). |
| PID namespace | **No** (`--unshare-pid` absent). `--proc /proc` (`lib.rs:685-686`) therefore shows host PIDs. | Not unshared. `ptrace`, `process_vm_*`, `kill`/`tkill`, `pidfd_*` are denied by seccomp (`seccomp.rs:239-310`). |
| network namespace | **No**. The guest shares the host netns: loopback, LAN, internet, and all abstract AF_UNIX sockets. | Not unshared by default. Seccomp allows only `AF_UNIX` sockets (`seccomp.rs:355-366`). |
| IPC / UTS / cgroup ns | **No** for all three. | Not unshared. `clone` with any `CLONE_NEW*` flag is denied (`seccomp.rs:88-97,416-420`). |
| cgroup limits | None in `acos-art`. Only the wrapper convention `tools/guarded.sh` (`guarded.sh:84-92`). | Probed only, not required (`confine.rs` header; `probe.rs`). |
| bind mounts | Host paths bound into the guest are listed in §3. Almost everything under `/data` is **rw**. | Landlock: read-only roots and one rw root, everything else denied (`landlock.rs:185-…`). |
| `/proc`, `/dev` | `--proc /proc` is the host PID namespace. `--dev /dev` is a fresh minimal `/dev` (`lib.rs:683-684`). | `/proc` and `/dev` are left as they are. Access is limited by Landlock/seccomp, not by mount views. |
| environment | **Inherited in full.** The builder only adds `--setenv` entries (`lib.rs:742-747`). No `--clearenv` and no `env_clear` anywhere in `acos-art` (grep). | `clearenv` and then exactly `ConfineProfile.env` (`linux/mod.rs:86-109`). |
| capabilities | No explicit drop (no `--cap-drop`). Relies on bwrap default **(verify)**. | No explicit drop, but `no_new_privs` plus seccomp `capset`/`setuid*` denial (`seccomp.rs:286-295`). |
| `no_new_privs` | Not requested by the argv. **(verify)** via `NoNewPrivs:` in `/proc/self/status` inside a sandbox. | First layer, `PR_SET_NO_NEW_PRIVS` (`linux/mod.rs:22-34`). |
| die-with-parent | **No** `--die-with-parent`. The launcher sets `PR_SET_PDEATHSIG` in `pre_exec` (`launcher.rs:219`, `app.rs:329`, `launch.rs:253`, `session.rs:471`). That reaches the bwrap process; whether it reaches `dalvikvm64` depends on bwrap exec/fork behaviour **(verify)**. | Not applicable (in-process). |
| `setsid` / session | `process_group(0)` only. A terminal SIGINT to `acos-art` does not reach the guest group. No `--new-session`. | Not applicable. |
| fd hygiene | None in the bwrap route. Rust `Command` only sets CLOEXEC on its own fds. | `close_range` for all fds above `keep_fds` (`linux/mod.rs:36-83`). |
| rlimits | `RLIMIT_CORE=0` and `RLIMIT_NOFILE=4096` always (`launcher.rs:229-232`; same in `app.rs:330-333`, `launch.rs:254-257`, `session.rs:472-475`). `RLIMIT_AS` only when `--rlimit-as-mb` is set, default 0 (`launcher.rs:216-228`). No CPU, NPROC, FSIZE, STACK or MEMLOCK limits. | AS 4 GiB, NOFILE 1024, NPROC 8192, CPU 60 s, FSIZE 2 GiB, STACK 256 MiB, MEMLOCK 64 MiB, CORE 0 (`confine.rs:102-117`). **Defaults are unusable for ART** (AS breaks Scudo; CPU 60 s is too short). |
| seccomp | **None.** `acos-art` never installs a filter, so the `SIGSYS` classification in `launcher.rs:393` only covers ART's own failures. | Default-allow deny list with about 80 syscalls, `EACCES` return, `AF_UNIX`-only sockets (`seccomp.rs:239-471`). Explicitly not validated on the ART boot path (`seccomp.rs:41-43`). |
| Landlock | **None.** | Read-only roots and one rw root. ABI≥6 scopes block signals and abstract UNIX sockets (`landlock.rs:55-58,153-157`). |
| memory | None (see §4). | None in the probe profile. Not required. |

Summary for Q1: the bwrap route gives a **mount view** (tmpfs root, explicit binds) and an
**unprivileged userns** (only via bwrap defaults). It does not isolate processes, the network or IPC,
and it does not constrain syscalls.

---

## 2. What is missing for untrusted APK native code

Status key: ✅ present, ⚠️ partial, ❌ missing.

| Need | Status | Evidence / what to add |
|---|---|---|
| seccomp filter on the ART process | ❌ | Nothing installs one. Use the arm64 pattern: `SeccompMode::DeferredPreload` (`confine.rs:70-79`, `:342-360`) with `acos-seccomp-guard` as an `LD_PRELOAD` constructor (`crates/acos-seccomp-guard/src/lib.rs:37`). Do **not** install it in the launcher's `pre_exec`, because `execve` is denied (`seccomp.rs:242-243`) and bwrap needs to exec. Verify ART's syscall set against the filter (`seccomp.rs:41-43`). |
| Landlock on top of bwrap | ❌ | Must be applied after bwrap has finished its mount setup. Applied in the parent it would block bwrap. Apply it in the same guest constructor as seccomp. Grant `/system`, `/apex/*`, and `/data` subtrees only. Granting `/proc` as a subtree still leaves `/proc/<pid>/root` pointing at the host root, which should be denied. **(verify)**. |
| rlimits | ⚠️ | Keep `RLIMIT_CORE=0` and `NOFILE`. Do **not** use `acos-sandbox`'s default profile for ART: AS=4 GiB breaks Scudo (`launcher.rs:64-67`), and CPU=60 s is too short. Add NPROC (ART creates threads, so use a generous cap) and FSIZE. |
| RLIMIT_AS | ❌ and unusable | Scudo reserves about 8.6 TB of VA at start-up (`launcher.rs:64-67`). Memory must be bounded by cgroup (§4). **Untested idea:** `RLIMIT_DATA` counts only private writable mappings on modern kernels, so it may work where AS does not. Test it before relying on it. |
| memory cgroup via `guarded.sh` | ⚠️ | `guarded.sh` gives `systemd-run --user --scope -p MemoryMax=… -p MemorySwapMax=0 -p TasksMax=512` (`guarded.sh:84-88`). But (a) it is a convention, not enforced by `acos-art`; (b) its fallback is `ulimit -v 8000000` (`guarded.sh:90`), which kills ART with a Scudo "NO MEMORY" error. `classify` then reports it as `HOST_MEMORY_LIMIT` (`launcher.rs:413-415`). The fallback must be removed for ART. |
| network off | ❌ | Add `--unshare-net`. The path-based sockets used by the launcher survive it (`logd.rs:81`, `launch.rs:164`, `session.rs:423`). Only abstract sockets and TCP/UDP are lost. |
| writable paths limited to the session data dir | ❌ | `ArtRoot::sandbox` binds the **whole** data dir rw (`lib.rs:680-682`). All sessions, runs, `img/`, `dalvik-cache`, `linkerconfig/` and `system/environ/` are shared (`lib.rs:172-186`). Bind only per-run `task/`, `app/`, `out/`, `session/<id>/`, and a socket dir. Everything else should be read-only or per-run tmp. |
| no access to host `/home` | ⚠️ | Hidden by `--tmpfs /`, so this holds for the mount view. It breaks if `/proc/<pid>/root` is readable (§3). `ACOS_ART_DATA` (`lib.rs:159-165`) can point the rw bind anywhere, including into `$HOME`. Reject that. |
| X / Wayland sockets | ⚠️ | Path sockets are hidden by the tmpfs root. The **abstract** X socket (`@/tmp/.X11-unix/Xn`) is reachable because the netns is shared. `DISPLAY` and `WAYLAND_DISPLAY` are inherited (no `clearenv`). `--unshare-net` plus `--clearenv` fixes both. |
| DBus | ⚠️ | Path sockets under `/run/user/<uid>` are hidden. `DBUS_SESSION_BUS_ADDRESS` is inherited, and abstract-address buses are reachable through the shared netns. Same fix. |
| die-with-parent / orphan prevention | ❌ | Add `--die-with-parent`. Kill the process group on `Drop`. A launcher crash today leaves the guest running, and **(verify)** that the 36 GB runaway may be this case. |
| env sanitisation | ❌ | Add `--clearenv` and an explicit `--setenv` list. `docs/ART_RUNTIME_PLAN.md:206-207` already requires "sanitise inherited LD_*/Java environment". |
| capabilities | ⚠️ | Add `--cap-drop ALL`. Confirm `CapEff: 0000000000000000` in a probe. |
| `no_new_privs` | ❌ | Set `prctl(PR_SET_NO_NEW_PRIVS)` in `pre_exec`. This is safe before bwrap, which is not setuid. **(verify)** that bwrap does not already set it. |
| `/proc` exposure | ❌ | Add `--unshare-pid`, then `--proc /proc` (already present). This makes `/proc` show only the sandbox's PIDs. |
| `/sys`, `/etc/passwd` | ⚠️ | `--ro-bind /sys` (`lib.rs:687-689`) exposes host sysfs and cgroup data. `/etc/passwd` (`lib.rs:694-696`) exposes user names. Both are low severity. Remove if ART boots without them. |
| host-side parsing of guest data | ❌ | The guest talks to host-side Rust code over `/dev/socket/logdw` (`logd.rs:79-118`, `parse_frame` at `:152-175`), `/data/acos-launch.sock` (`launch.rs:163-186`, Binder peer parsing guest parcels in-process), and reads host-parsed files: `system/environ/classpath` (`lib.rs:289-298`), `tree.json`/`frame.png` (`app.rs`). These must be treated as hostile input. |

---

## 3. Host sockets, devices and paths reachable from inside (from the argv, not run)

All paths below come from `Sandbox::command` (`lib.rs:657-773`) plus the per-route `ro_bind`/`rw_bind` calls.

**Bind mounts into the guest**

| Guest path | Host source | Mode | Ref |
|---|---|---|---|
| `/system` | `<sysroot>/system` | ro | `lib.rs:664-665` |
| `/apex/<module>` | `<sysroot>/system/apex/<name>` (per module) | ro | `lib.rs:671-676` |
| `/apex/apex-info-list.xml` | `<data>/apex-info-list.xml` | ro | `lib.rs:669-670` |
| `/linkerconfig` | `<data>/linkerconfig` | **rw** | `lib.rs:677-679` |
| `/data` | **whole `<data>`** | **rw** | `lib.rs:680-682` |
| `/dev/socket` | `<data>/socket` (contains host `logdw`) | **rw** | `lib.rs:725-728`, `app.rs:317`, `launch.rs:236`, `session.rs:451` (per-session `socket/`) |
| `/data/shim/libartshim.so` | `<data>/shim/libartshim.so` | ro | `lib.rs:721-724` |
| `/data/img` | `<image dir>` | rw | `lib.rs:758-762`, `main.rs:530` |
| `/etc/passwd` | host | ro | `lib.rs:694-696` |
| `/sys` | host sysfs | ro | `lib.rs:687-689` |
| `/dev` | bwrap fresh minimal `/dev` (null, zero, full, random, urandom, tty, `/dev/shm`, pts) | – | `lib.rs:683-684` |
| `/proc` | **host PID namespace** | – | `lib.rs:685-686` |
| per-run DEX | `<data>/task/*.dex` | ro | `launcher.rs:194` |
| per-run ART shim / substrate / driver | `<data>/shim`, `<data>/app/libacosart.so`, `/data/lib/libacos_binder_driver.so` | ro | `app.rs:315-316`, `launch.rs:233-235` |

**Host sockets reachable from the guest**

- `/dev/socket/logdw` → host `Logd` datagram reader (`logd.rs:79-118`).
- `/data/acos-launch.sock` → host Binder peer (`launch.rs:164,186`, `E3G_SOCK` at `launch.rs:242`).
- Per-session `/dev/socket/logdw` → host reader (`session.rs:423-425`).
- Session stdin/stdout pipes → host protocol parser (`session.rs:486-500`).

**Host sockets reachable through the shared network namespace**

- Every abstract AF_UNIX socket on the host, including the abstract X11 socket if the user runs X and a D-Bus abstract bus if one exists.
- TCP/UDP to `127.0.0.1` (any local dev server, the Paseo daemon, etc.), to the LAN, and to the internet.

**Environment reachable (inherited, not cleared)**

- `DISPLAY`, `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, `SSH_AUTH_SOCK`, `PATH`, `HOME`, and any API keys the launcher was started with. `Command` inherits the parent environment (`lib.rs:659`, `launcher.rs:238`).

**Process view**

- `/proc` shows all host processes of the same uid. Without Yama restrictions on the read-mode paths, the guest can read `/proc/<pid>/environ`, `/proc/<pid>/cmdline` and `/proc/<pid>/fd`.
- **Probable host-root escape:** `/proc/<pid>/root` is opened with `PTRACE_MODE_READ`. Yama `ptrace_scope=1` (read from `/proc/sys/kernel/yama/ptrace_scope`) restricts only `PTRACE_MODE_ATTACH`. So for any dumpable same-uid process, including the launcher itself, `/proc/<pid>/root` should resolve to the host root and bypass the tmpfs root. This applies to the launcher and to the user's shell, daemons and editors. **(verify)** with a probe that opens `/proc/<launcher-pid>/root` inside the sandbox after the hardening in §5 #1 is applied, and records the result. `ptrace` is blocked only if seccomp is added, and it is not. Landlock would also deny this path if the rule set is correct (§2).

**Devices**

- No host device is bound. `/dev` is minimal. `/dev/dri`, `/dev/kvm`, `/dev/snd`, `/dev/input` are absent. `/sys` (ro) exposes device metadata only.
- `/dev/shm` is a bwrap tmpfs inside the sandbox (writable, RAM-backed) **(verify)**.

**Host filesystem**

- Hidden by `--tmpfs /` except for the binds above.
- Reachable through `ACOS_ART_DATA` if someone points it into `$HOME` (`lib.rs:159-165`).
- Reachable through `/proc/<pid>/root` (see above).

---

## 4. Memory exhaustion exposure

Today a runaway `acos-art` reached 36 GB. Nothing in `acos-art` would have stopped it:

- `RLIMIT_AS` is 0 by default for every route (`launcher.rs:89`; `app.rs`, `launch.rs`, `session.rs` never set it).
- The only memory cap, `tools/guarded.sh`, is a convention. It does not run automatically, and its fallback `ulimit -v 8000000` is wrong for ART (`guarded.sh:90`).
- No cgroup is created by `acos-art`. The only cgroup-level guard is `guarded.sh` with `MemoryMax=4G` (`guarded.sh:87`), and only if the user invokes it.
- The `ACOS_HOST_RSS_MAX` guard is in the Rust VM only.

**Unbounded host-side buffers** (these grow in the launcher, not in ART, so they are unaffected by ART's own heap limits):

| Buffer | Ref | Risk |
|---|---|---|
| bwrap stdout/stderr `read_to_end` | `launcher.rs:250,257`; `app.rs:352,358`; `launch.rs:273,279` | Guest log/print flood grows the buffer until timeout. |
| `Logd` entries `Vec` | `logd.rs:85,96` | Every datagram is pushed; no cap. A flood of datagrams from the guest grows the Vec. |
| session stdout `BufReader::lines()` | `session.rs:496-500` | A single line with no newline grows a `String` without bound. |
| Binder peer receive path | `launch.rs:163-186` | Guest-controlled parcels parsed in-process; size limits not checked here **(verify)**. |

**Guest-side exposure without caps:**

- Native heap or JNI allocations. `-Xmx` only bounds the Java heap, and the wall-clock timeout (60–120 s by default) is the only brake.
- `/data` disk writes. `RLIMIT_FSIZE` per file is not set in the bwrap route; the total is unbounded.
- `/dev/shm` (RAM-backed, see §3).

**Suspects for the 36 GB run:** the repo does not record the incident, so I cannot attribute it. Candidates by code evidence are (1) a native or JNI allocation loop, (2) stdout/stderr flood through `read_to_end`, (3) a `Logd` flood, and (4) the launcher surviving its parent, which is an orphan that nothing kills (§2, die-with-parent). Caps for all four are in §5.

**Caps needed before running third-party APKs:**

1. A hard cgroup v2 limit on each run: `memory.max` (e.g. 4 GiB), `memory.swap.max=0`, `pids.max`. Use `systemd-run --user --scope` like `guarded.sh`, or a delegated cgroup, and **fail closed** if neither is available.
2. A cap on every host buffer (suggested: 8 MiB per stream; `Logd` 100k entries with a drop counter; session line length 64 KiB).
3. Kill the whole process group on launcher exit (`--die-with-parent` plus a `Drop` guard).
4. A watchdog: `tools/ramguard.sh` already exists. Make the ART route check `MemAvailable` before starting.
5. Remove the `ulimit -v` fallback for ART runs.

---

## 5. Prioritised hardening (effort: S ≤ 1 h, M ≈ 1 day, L ≈ several days)

Blocker = required before running third-party APKs.

| # | Change | Where | Effort | Blocker | Why |
|---|---|---|---|---|---|
| 1 | Add `--unshare-user --unshare-pid --unshare-net --unshare-ipc --unshare-uts --unshare-cgroup-try --die-with-parent --new-session --cap-drop ALL --disable-userns --clearenv` with an explicit `--setenv` list. Keep `--proc /proc` after `--unshare-pid`. | `lib.rs:657-773` | S | **yes** | Closes host PID/net/IPC view, `/proc/<pid>/root` escape, env leak, X/DBus, orphan survival, nested userns. Verify the 27-program corpus still passes (`acos-art verify`). |
| 2 | Per-run `/data` tree. Bind only `task/`, `app/`, `out/`, `session/<id>/`, and a per-run socket dir rw. Make `img/`, `linkerconfig/`, `system/environ/`, `shim/` read-only, or regenerate them per run into a fresh dir. Host must not parse guest-writable files. | `lib.rs:172-186,240-298,677-682` | M | **yes** | A guest can overwrite `data/img/x86_64/boot.art` (auto-loaded via `-Ximage`, `launcher.rs:169-176,201-203`), `system/environ/classpath` (read by the host and passed as `BOOTCLASSPATH`, `lib.rs:289-298,737-747`), and `linkerconfig`. These persist across runs and sessions. |
| 3 | Cgroup memory limit for every ART launch, and fail closed without one. Remove the `ulimit -v` fallback for ART. | `launcher.rs`, `app.rs`, `session.rs`, `launch.rs`; `guarded.sh:84-92` | S–M | **yes** | §4. |
| 4 | Cap every host-side buffer (stdout, stderr, `Logd`, session lines, Binder peer). | `launcher.rs:248-259`, `app.rs:350-361`, `launch.rs:268-281`, `logd.rs:85-96`, `session.rs:494-500` | S | **yes** | §4. |
| 5 | Seccomp deny-list on the ART process via the deferred `acos-seccomp-guard` `LD_PRELOAD` constructor. Validate ART against it on the corpus, then the `render-app` and `launch` paths. | `confine.rs:70-79,342-360`; `crates/acos-seccomp-guard` | L | **yes** | Blocks `ptrace`, `mount`, `unshare`, `setns`, `io_uring`, `bpf`, non-AF_UNIX sockets, `execve`. The filter has not been validated on the ART boot path (`seccomp.rs:41-43`). |
| 6 | Landlock ruleset applied in the same guest constructor, after bwrap's mount setup. Grant `/system`, `/apex/*`, `/data/<run>` only. | `landlock.rs:185-…`; new guard hook | M (after #5) | **yes** | Stops `/proc/<pid>/root` and any path outside the granted set, even if seccomp is bypassed **(verify)**. |
| 7 | `--tmpfs /dev/shm` or size cap, and bind `--size` on tmpfs if the bwrap version supports it. | `lib.rs:683-684` | S | no | RAM-backed writes (§3). |
| 8 | Set `NO_NEW_PRIVS` in `pre_exec` and confirm `NoNewPrivs: 1`. | `launcher.rs:218-233` and siblings | S | no | Defence in depth. |
| 9 | rlimits for the ART route: keep CORE=0 and NOFILE=4096; add FSIZE and NPROC; do not use `acos-sandbox`'s default profile. Test `RLIMIT_DATA` as the AS substitute. | `launcher.rs:216-232` and siblings | S (+test) | no | §2. |
| 10 | `ACOS_ART_DATA` must resolve under the repo cache (reject `$HOME` and `/`). | `lib.rs:159-165` | S | no | §3. |
| 11 | Remove `--ro-bind /sys` and `/etc/passwd` unless ART needs them. | `lib.rs:687-696` | S | no | Low-severity exposure. |
| 12 | Fix `--rlimit-as-mb` default: `unwrap_or(8192)` on a bad parse silently sets an AS limit that breaks ART. Make it an error. | `main.rs:121` | S | no | Silent misconfiguration. |
| 13 | Harden the host-side parsers: `parse_frame`, `longest_printable_run`, Binder peer, classpath parser, `tree.json` reader. Fuzz them. | `logd.rs:152-199`, `launch.rs:163-186`, `lib.rs:300-320`, `app.rs` | M | no (before a public release) | Guest input reaches these parsers at host privilege. |
| 14 | Adversarial corpus: a DEX/native test set that tries the escape paths (`/proc/*/root`, abstract sockets, `ptrace`, boot-image overwrite, log flood, stdout flood, native allocation loop). Gate on each. | `crates/acos-art/tests`, `results/art-runtime` | M | **yes** | No adversarial tests exist today. The 27-program corpus is functional only. |

**Minimum before running any third-party APK:** items 1–6, plus 4 (buffer caps, which
also close the flood vectors). Items 7–14 are needed before a general release.

**Interim rule (until 1–6 land):** run only APKs the user built or fully trusts, and run them only
under `tools/guarded.sh --mem 4G --time 120` with `ACOS_ART_DATA` pointed at a throwaway directory.
That still leaves the network, PID and env exposure in §3, so it is not safe for untrusted input.

---

## 6. Open items and what would settle them

- **bwrap version and defaults** (no_new_privs, `--size` on tmpfs, whether the guest is exec'd or forked, `--disable-userns`): run `bwrap --version`, then a probe that reads `/proc/self/status` (`NoNewPrivs`, `CapEff`, `Seccomp`) inside the sandbox. Not run here.
- **`/proc/<pid>/root` escape**: a probe that opens `/proc/<launcher-pid>/root` from inside the sandbox, first with the current argv and then after #1. Not run here.
- **Orphan survival**: kill the launcher with SIGKILL during a run and check whether `dalvikvm64` stays alive. Not run here.
- **Memory attribution of the 36 GB run**: the incident log is not in the repo. Check `results/tmp/guarded.log` if it exists, or the launcher's stdout/stderr buffers, to decide which suspect in §4 it was.
- **Scudo and RLIMIT_DATA**: confirm ART starts under `RLIMIT_DATA` before relying on it.

Nothing in this review changes code. The only file written is `results/art-runtime/S1-sandbox-review.md`.
