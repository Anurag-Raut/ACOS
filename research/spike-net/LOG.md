# SPIKE-NET log (ADR-0030 gate: real network through pasta)

Worktree: `/home/Qui/.t3/worktrees/acos/t3-2f28c6b2-bee01c021cecb8a6`, branch `t3/spike-net`.
Host: no passt/pasta installed, no sudo, no kernel changes. All spike code is gated on
`ACOS_SPIKE_NET=1`; with the variable unset every code path is the previous one.

Every attempt is recorded, including the ones that failed and what they changed.

---

## 1. Build pasta from the official source (static)

Attempt 1 — release tarball:
`curl https://passt.top/passt/snapshot/passt-2025_09_19.409c86c.tar.gz` → **404** (likely
snapshot name). Abandoned the tarball route.

Attempt 2 — git mirror:
`git clone --depth 1 https://passt.top/passt` → OK, HEAD `7380393` (2026-11-04).
`make static -j8` → OK, but `pasta --version` printed `unknown version` (no tag in a
shallow clone). A binary whose version cannot be reproduced is not an installer input.

Attempt 3 — pinned release tag:
```
git fetch --depth 1 origin refs/tags/2026_10_02.cba3570:refs/tags/2026_10_02.cba3570
git checkout 2026_10_02.cba3570          # cba3570, 2026-09-28
make clean && make static -j8 VERSION=2026_10_02.cba3570
```
Result (`research/spike-net/artifacts/passt-build.txt`):

| field | value |
|---|---|
| source | passt project, tag `2026_10_02.cba3570`, commit `cba357068dc586e7540971eb40bb56c5e62c4de1` |
| build | `make static VERSION=2026_10_02.cba3570` (glibc static, `-static -pie`) |
| version banner | `pasta 2026_10_02.cba3570` |
| size | 1,548,232 bytes (statically linked, x86-64) |
| sha256 | `314c37e878bd6ffe43200735800d5efd4859d81d46b0450358a103c36c0edf60` |
| licence | GPL-2.0-or-later (`LICENSES/GPL-2.0-or-later.txt`); `LICENSES/BSD-3-Clause.txt` for the bundled `siphash`/compat bits |
| install copy | `.cache/spike-net/bin/pasta` (spike only; the installer packaging is v0.4.0 work) |

Note: `passt` and `pasta` are the same binary (pasta is `/proc/self/exe`-basename
dispatched); the release build also emits `passt.avx2`, which the binary prefers and
falls back from with a one-line warning when absent.

---

## 2. Attach pasta to the session's network namespace

Attempt 1 — `pasta -f -q --no-map-gw -- <cmd>`: the command ran in a fresh netns
(`net:[4026533570]` vs host `net:[4026531833]`) but with **only `lo` and no routes**:
`connect 93.184.215.14:443 -> Network is unreachable`. Cause: pasta does not configure
addresses/routes unless `--config-net` is given.

Attempt 2 — add `--config-net`: internet worked, DNS worked, **but `127.0.0.1:<host
port>` was reachable from the guest**:
```
connect 104.20.23.154:443 -> OK
connect 127.0.0.1:18080 -> OK          <-- the host's `python3 -m http.server`
RESULT internet=reachable host_loopback=REACHABLE_BAD lan=blocked dns=ok
```
Root cause, from pasta(1) and `--help`: `-T/--tcp-ns` and `-U/--udp-ns` default to
**`auto`**, i.e. pasta mirrors every port the host has bound on loopback into the
namespace, and the local-traffic "bypass path" splices layer-4 sockets between the two
namespaces (`--no-splice` disables it). This is exactly the leak ADR-0030 §3 forbids
(`acos view`, the MCP daemon, everything on 127.0.0.1). Also visible from inside: the
guest saw the host's listeners in `ss -ltn` (`*%lo:53`, `*%lo:22`, `*%lo:18080`, ...).

Attempt 3 — harden the invocation (`-T none -U none --no-splice --map-host-loopback
none`, and give the guest a link-local address instead of the host's LAN address):
```
connect 127.0.0.1:18080 -> Connection refused     <-- fixed
connect 192.168.31.1:80 -> OK                     <-- LAN still reachable
```
So loopback is closed, but **the LAN is not**: pasta's NAT connects from the host's
routing table, so any destination the guest names (including the LAN gateway) is reached
from a host socket. passt has no destination ACL. ADR-0030 §3 ("no route to … the LAN
gateway by default") is therefore not implementable with pasta flags alone.

Attempt 4 — enforce the LAN block inside the session's own netns: the netns belongs to
pasta's user namespace, so the command pasta spawns is root **in that netns** and has
`CAP_NET_ADMIN` there (and nothing else). `iptables -A OUTPUT -d 192.168.0.0/16 -j DROP`
succeeded (`IPT_OK`). Full check with the LAN guard in place
(`research/spike-net/artifacts/probe-hardened.txt`):
```
public tcp:      connect 104.20.23.154:443 -> OK
host loopback:   connect 127.0.0.1:18080   -> Connection refused
lan gateway:     connect 192.168.31.1:80   -> Connection timed out
udp dns:         1.1.1.1:53 -> OK (n=61)
RESULT internet=reachable host_loopback=blocked lan=blocked dns=ok
```
(Host side of the test: a throwaway `python3 -m http.server 18080 --bind 127.0.0.1`,
started for the probe and stopped after it.)

Result: **transport GO** with three flags pasta's defaults do not give you
(`-T none -U none --no-splice`) plus an in-namespace RFC1918/169.254 DROP chain.

PID/`--netns` mode was not used: pasta's own "run a command in a new netns + userns"
mode (`pasta [OPTIONS] -- CMD`) is what the spike integrates, because bwrap cannot join
an existing netns and an unprivileged parent cannot `setns()` into a netns owned by a
child user namespace.

---

## 3. seccomp: AF_INET/AF_INET6 only with the spike on

`crates/acos-art/src/seccomp.rs`: the socket branch now reads
`AF_UNIX | (AF_INET, AF_INET6 if allow_inet)`; `allow_inet` comes from
`spike_net::enabled()`. Unit call sites updated (`build_program_with(action, allow_inet)`),
so the default filter program is bit-identical to before (same instruction sequence,
`allow_inet = false`).

## 4. Session sandbox under pasta

`crates/acos-art/src/lib.rs`: `Sandbox::command` emits the flag list from
`spike_net::sandbox_flags()` — the production list with **`--unshare-net` removed** when
the flag is on — and wraps the finished bwrap argv as
`pasta <hardened args> -- /bin/sh -c '<LAN guard>; exec "$@"' sh bwrap …`.
Every other namespace flag, `--die-with-parent`, `--clearenv`, `--cap-drop ALL` and the
guest uid mapping stay.

Attempt — full ART session with the flag on:
```
ACOS_SPIKE_NET=1 acos launch fixtures/build/apps/hello-views.apk
launch: reached first_frame (classes=2764, wall=5172ms)
  [ok] apk_parsed … process_booted … first_frame       (all 8 stages ok)
```
Evidence: `research/spike-net/artifacts/art-boot-pasta.txt`. So pasta's userns, bwrap's
nested userns and the application uid (`10192`) compose: the guest boots with the guest
uid inside a namespace that has a real NATed interface.

---

## 5. DNS: the dnsproxyd protocol

The AOSP cache in this checkout is a sparse bionic tree (`libc/dns/` is not present), so
the resolver side was fetched from googlesource (android-14.0.0_r1):
`bionic/libc/dns/net/getaddrinfo.c` (`research/spike-net/artifacts/bionic-getaddrinfo.c`).

Findings:
* bionic does **not** read `/etc/resolv.conf`. After its own numeric/hosts attempts,
  `getaddrinfo()` calls `android_getaddrinfo_proxy()`, which opens
  `__netdClientDispatch.dnsOpenProxy()` — `/dev/socket/dnsproxyd` — and writes a
  **NUL-terminated ASCII** request:
  `getaddrinfo <host|^> <serv|^> <flags> <family> <socktype> <protocol> <netid>`.
* The answer is `"222\0"` (`DnsProxyQueryResult`) followed by, per address:
  `BE32 1, BE32 ai_flags, ai_family, ai_socktype, ai_protocol, BE32 addrlen, sockaddr
  bytes, BE32 namelen, name`, terminated by `BE32 0`. A non-222 code plus one more
  4-byte code means failure → bionic returns `EAI_NODATA`.
* `gethostbyname()` in the same file goes through the same proxy.

Implemented: `crates/acos-art/src/spike_net.rs::serve_dnsproxyd` — a minimal responder
bound at `<session socket dir>/dnsproxyd` (which the sandbox already binds over
`/dev/socket`, so it appears at the device path), answering `getaddrinfo` with the host's
`libc::getaddrinfo`. `gethostbyname` is answered as a structured error, deliberately, and
said so in the report.

CA store: the image **has** it — `.cache/art/sysroot/system/etc/security/cacerts`
(134 hashes) plus `cacerts_google`. Nothing to add for step 4's TLS requirement.

---

## 6. Connectivity

`crates/acos-services/src/connectivity.rs`: `default_capabilities_bits()` adds
`NET_CAPABILITY_VALIDATED` only when `ACOS_SPIKE_NET=1`. Default behaviour (no
`VALIDATED`, `isConnected=true`/`isInternetReachable=false`) is unchanged.
`LinkProperties` still carries no addresses/DNS servers — noted as remaining work.

---

## 7. What was not measured

* No in-app HTTPS proof (step 6): no fixture APK performs a request, and building one
  (`tools/build-apps.sh` + a new `fixtures/apps/net-check`) plus a scenario run did not
  fit the one-hour budget. The transport, the namespace, the seccomp gate and the DNS
  protocol are proven separately; the app-level round trip is not.
* No A/B metrics (step 7): cold/warm launch, per-session memory and the N=20 density
  check were not run with flag off vs flag on, so there is **no** evidence that `real`
  can be the default. Per the owner's condition the recommendation is therefore `off`.

---

# Session 2 (same branch `t3/spike-net`, new worktree) — boundary, fixture, gate

Session 2 started from `4638d9f4` with three decisions from the orchestrator and one
instruction each: (1) the LAN block must be a *boundary*, not a policy the guest can undo;
(2) an app-level HTTPS proof through `acos verify --engine art`; (3) the A/B gate, then the
default strictly from it.

## 1. The LAN block becomes a boundary

Session 1 installed the DROP rules from the same `/bin/sh` that then `exec`'d bwrap and
left the guest with `--cap-drop ALL`. That was already almost a boundary, but nothing
proved it, and the shim was fail-open (`iptables ...; exec bwrap` — a missing `iptables`
silently ran the guest with *no* LAN block).

Now (`crates/acos-art/src/spike_net.rs`, `boundary_script()`):

* the chain is `acos-net-drop` (RFC1918 + `169.254/16` + `100.64/10`), jumped from
  `OUTPUT`, installed **and verified rule by rule** (`iptables -w -C`) before bwrap runs;
* any failure — no `iptables`, chain creation, one rule, the OUTPUT jump — exits `111`
  and the guest is never started (fail closed, proven by the second test);
* the shim dumps the chain before and after the guest, so "the guest left it alone" is a
  diff of two dumps, not a claim;
* the shim records its own `netns`/`userns` inode and `CapEff` (it is root in pasta's
  user namespace, `CapEff=000001ffffffffff`) as the owner side of the comparison.

Guest side, measured in the real mount view
(`crates/acos-art/tests/spike_net_boundary.rs`, `results/handoff/SPIKE-NET/boundary/`):

```
probe: uid 10192
probe: cap-eff 0000000000000000                    <- no capability at all
probe: userns 4026533637   (shim/netns owner: 4026533566)   <- child userns
probe: socket-netlink DENIED:EACCES                <- seccomp blocks AF_NETLINK
probe: so-mark DENIED:EPERM                        <- no CAP_NET_ADMIN over the netns
probe: socket-raw-inet DENIED:EPERM
probe: iptables-flush exit=1
probe: iptables-flush-output iptables: Failed to initialize nft: Permission denied
probe: connect-public OK                           <- the feature still works
probe: connect-lan Operation now in progress       <- DROPped (connect times out)
probe: connect-host-loopback Connection refused    <- `-T none --no-splice`
probe: done
```

and the shim's two dumps are byte-identical (rules untouched, guest exit 0). Host-side
`/proc/<guest>/status` was read while the guest ran: `CapEff` is zero there too, and the
guest's user namespace is not the netns owner's.

`SO_MARK` is the capability probe on purpose: the kernel checks it against
`sock_net(sk)->user_ns`, i.e. the netns's owner namespace — exactly the right the guest
would need to rewrite the chain.

## 2. The seccomp `AF_INET` branch was dead code (found here, fixed)

Session 1 reported "seccomp allows `AF_INET`/`AF_INET6` only with the flag" and moved on.
The branch it added came *after* `jeq(AF_UNIX, list, deny)`, so every non-`AF_UNIX` family
went to the deny terminal first: with `ACOS_SPIKE_NET=1`, `socket(AF_INET, ...)` still
returned `EACCES`. Session 1 never noticed because its transport probe ran inside the
netns but *outside* bwrap (no seccomp), and an ART boot reaches first frame without a
socket of its own.

Fixed: the family gate now falls through `AF_UNIX → AF_INET → AF_INET6` to the syscall
deny list and denies everything else. `seccomp::tests::socket_families_follow_the_spike_flag`
runs the emitted cBPF program through a small interpreter and would have failed before the
fix (`AF_INET` → EACCES) and fails if `AF_NETLINK` ever becomes reachable.

## 3. Fixture `fixtures/apps/net-check` (items 4/5/6)

`HttpURLConnection` GET of `https://example.com` on a background thread; `TextView id=status`
shows `HTTP 200` or the exception class name (never a fake success); `id=connectivity` shows
the `ConnectivityManager`/`NetworkCapabilities`/`LinkProperties` report. Scenario
`scenarios/net-check.yaml` asserts `HTTP 200`, `internet=true validated=true` and the
`NetCheck` log line. Built with `ACOS_APP_ONLY=net-check tools/build-apps.sh`
(`fixtures/build/apps/net-check.apk`, 12 756 bytes). One API mismatch on the way:
`LinkProperties.getAddresses()` is not in the API-34 stub; `getLinkAddresses()` is.

## 4. What session 2 did *not* run, and why

* `acos verify --engine art` on `net-check` (items 4/5/6 at app level): not run. It needs
  a release `acos` build (built, `1m12s`) and then an ART session whose artifacts are
  0.5–2 GB (`results/handoff/ARTGC.md`); the host disk crossed the gate in §4 of
  COMMON_RULES (81 % → 87 % during this session) before the run could start.
* The A/B gate: not run (same disk gate, and the session's remaining budget). No timings
  were produced, so the default stays `off`.
* `LinkProperties` addresses/DNS/routes: still empty (`connectivity.rs` unchanged);
  item 5 remains PARTIAL.

# Session 3 (branch `t3/spike-net-session-2-gate`) — the app-level proof, parity, and the A/B

Started from `9cac941f` (session 2). Goal: finish the ADR-0030 gate — items 4/5/6 at
app level, item 5 parity, item 7's A/B — and set the default strictly from the A/B.

## 1. Rebuild the pieces the worktree did not have

The worktree is fresh, so: `cargo build --release -p acos-cli -p acos-art` into
`.cache/sn3-target` (1 m 10 s + 53 s), `pasta` re-built from the pinned tag
(`make static VERSION=2026_10_02.cba3570`, 6.5 s, sha256 identical to session 1's
`314c37e8…`), `net-check.apk` re-built (12 756 B).

First run failed on the scenario, not the network: `engine: art` is not in the
scenario loader's vocabulary (`native-preferred | native-only | reference-only`).
That is v0.3.0 track B's known CLI/YAML mismatch; the CLI `--engine art` selects
the engine, so the field is gone from `scenarios/net-check.yaml`.

## 2. Item 4: why the resolver was dead — the wrong `/dev/socket`

With `ACOS_SPIKE_NET=1` the app reported
`java.net.UnknownHostException: … No address associated with hostname`
(`EAI_NODATA`). Instrumenting the responder (env-gated request log) showed **zero
connections**: bionic never reached the socket.

A guest-side probe in the fixture (new `id=dnsprobe`, kept — it is what found
this) said: `exists=false`, and its own `System.loadLibrary("netd_client")` failed
with `… /system/lib64/libnetd_client.so … is not accessible for the namespace
"clns-2"` — that error is the *app's* classloader namespace refusing a non-public
library, i.e. the probe's own attempt, not libc's internal `dlopen`.

Reading where `/dev/socket` comes from settled it: **four** different directories
are bound at `/dev/socket`, and the verify path uses
`launch.rs`'s `sockets.0` (`<plan>/<tag>/sock` under the production zygote, a short
`/run/user/<uid>/…` dir otherwise), not the session launcher's `sock_dir` where
session 1 had put the responder (`session.rs`). So `/dev/socket/dnsproxyd` did not
exist in the app's mount view, bionic's `android_getaddrinfo_proxy` failed to
`connect()` and `getaddrinfo` answered `EAI_NODATA` — exactly the "No address
associated with hostname" the app showed.

Fix, at the level of the class rather than the one site: `serve_dnsproxyd` now
returns a `DnsProxy` handle (Drop stops the accept loop via a self-connect and
removes the socket) and is started in **every** directory that becomes
`/dev/socket` — `launch.rs` (the verify/live path), `session.rs`, `app.rs`
(`render-app`) — through `serve_dnsproxyd_if_enabled`, which is a no-op without
the flag. The socket is `0666` like the device's (`srw-rw-rw-`; SELinux, not DAC,
restricts it on Android), with the reasoning in the code.

Result (evidence `results/handoff/SPIKE-NET/net-check/on/`):

```
NetCheck: connectivity active=100 internet=true validated=true iface=wlan0 \
          addrs=1 dns=1 dnsList=1.1.1.1 routes=1
NetCheck: dnsprobe dnsprobe exists=true canRead=true canWrite=true …
NetCheck: body-first-bytes-len=256
NetCheck: fetch result=HTTP 200
dnsproxyd request log: req="getaddrinfo example.com ^ 1024 0 1 0 0" -> 222 answers=3
```

`acos verify --engine art` → `pass` (10 steps). Item 6 done; DNS/TLS through the
image's CA store work with no extra CA work.

## 3. Item 5 parity: `LinkProperties`

The app's report was `iface=wlan0 addrs=0 dns=0 routes=0`. `connectivity.rs` now
returns the namespace's real interface behind the flag: `169.254.2.15/24`, DNS
`1.1.1.1`, default route `0.0.0.0/0 via 169.254.2.1 dev wlan0`, DHCP server
`169.254.2.1`. One source of truth: those addresses are constants in
`acos-services::connectivity`, and `spike_net::pasta_args()` formats its
`-a/-g/-D` flags from them, so pasta cannot configure one interface while apps
read another.

The wire side needed real codec work, not a stub: `FieldOp::AddressList` (int
count + `writeByteArray` per address, with the IPv6 scope-id form) and specs for
`android.net.LinkAddress`, `android.net.RouteInfo`, `android.net.IpPrefix`; the
`LinkProperties` spec now has variable-length lists where it had three `Int(0)`
counts. Without the flag every list is empty, and an empty list writes the same
`0` the old `Int(0)` wrote, so the flag-off bytes are unchanged.

Now `addrs=1 dns=1 dnsList=1.1.1.1 routes=1` and the scenario asserts it.

## 4. Flag off: the honest failure is a `SecurityException`, not `UnknownHostException`

With the flag unset (new `scenarios/net-check-offline.yaml`, which asserts the
failure so "off still works" is a verdict):

```
connectivity … internet=true validated=false iface=wlan0 addrs=0 dns=0 dnsList=none routes=0
dnsprobe exists=false …
fetch failed: java.lang.SecurityException: Permission denied (missing INTERNET permission?)
fetch result=java.lang.SecurityException
```

Session 2 predicted `UnknownHostException`/`SocketException`; the measured class
is `java.lang.SecurityException`, because the seccomp filter denies
`socket(AF_INET)` with `EACCES` and libcore's `IoBridge` turns that into
`SecurityException: Permission denied (missing INTERNET permission?)` before any
name is resolved. The scenario accepts either, and the report says which happened.
`acos verify` → `pass` (9 steps): no fake success, no VALIDATED bit, no socket.

## 5. Tests (each fails on the code before this session)

* `acos-art --lib dns_tests::dnsproxyd_socket_is_world_writable_and_answers` — the
  socket mode is 0666 (the measured first wall), a real bionic request gets
  `222` + one address, and dropping the handle removes the socket.
* `acos-art --lib dns_tests::pasta_args_use_the_connectivity_constants` — the
  drift guard for the addresses above.
* `acos-services --lib link_properties_tests::{flag_on_reports_…,
  flag_off_keeps_every_list_empty, validated_capability_follows_the_flag}`.
* Whole `acos-services` suite green (86 lib + every integration test), plus the
  `parcelable` DEX oracle test (which is what proves the new spec orders).

## 6. Item 7: the A/B gate — the bench harness is broken at this commit

`tools/bench/acos-bench.sh` (with `--no-build`, my own target dir and scratch, and
`ACOS_ART_BIN`/`ACOS_SPIKE_NET*` exported) fails on this branch **and on `main`**
before any number is produced:

| defect | where | state |
|---|---|---|
| `NameError: _environ_has` | `tools/art-runtime/d2_warm.py:363` (`survivors`) | **fixed here** (Python twin of `bench_environ_has`) |
| `NameError: _record_daemon` | `d2_warm.py:314` (`Serve.__init__`) | **fixed here** (appends `pid starttime` to `ACOS_BENCH_PIDFILE`) |
| `UnboundLocalError: cohort` | `tools/bench/acos_bench.py` `phase_loop` | **fixed here** (initialise before the `try`; it masked the real error) |
| `RuntimeError: two consecutive settled captures differ: [None, None]` | `phase_latency` | **open** — fails in 4.8 s on the *off* arm too |
| `RuntimeError: reference tree is not the count-0 screen: count='0' nodes=14` | `phase_cold` | **open** — the stored reference tree no longer matches the current rendering |

So no M3/M4/M5/M6/M7/M8, no M2 PSS and no N=20 cell were produced, on either arm.
Fixing the last two would mean re-baselining a reference tree and changing what
"settled" means — both are other tracks' decisions (rel3 settle, PERF-REL3), not
this spike's, and re-baselining to make a gate pass is exactly what the rules
forbid.

## 7. What the flag costs, measured without the bench

Lean, interleaved substitute on the launch path (same binary, same scenario,
alternating arms, `acos verify --engine art` on `hello-views`, `.cache/ab/lean-ab.sh`):

| measurement | flag off | flag on |
|---|---|---|
| launch step wall (`steps[0].wall_ms`, 2 reps/arm) | 155, 155 ms | 154, 155 ms |
| ART session boot (`session_start_ms`) | 177, 177 ms (one cold image build: 3894) | 176, 219 ms |
| session cgroup **peak** (`hello-views`) | 108, 109 MiB | 144, 147 MiB |
| session cgroup **peak** (`net-check`) | 74 MiB | 104 MiB |

Direct per-session costs of the boundary (measured, not inferred):

* `pasta` process: **27.6 MiB RSS**, 1 thread (`/proc/<pid>/status` of the real
  binary with the production flags; it re-execs into `passt.avx2`).
* the shim's `/bin/sh`: ~4 MiB RSS, resident for the whole session (it is what
  writes the after-dump).
* the whole chain install **and** verification (1 `-N`, 5 `-A`, 1 `-C`/`-A` on
  `OUTPUT`, 5 `-C`, 1 `-C` on the jump): **7 ms**; 10 × `iptables -S` = 6 ms, so
  the **before+after dumps (4 of the 18 `iptables` execs) cost ≈ 2.4 ms**.
* pasta + shim end to end (netns/userns setup + chain + 2 dumps): **31, 36, 42 ms**
  wall per session.

So the after-dump is **not** what costs: the cost is `pasta` itself (~28 MiB RSS,
one process per session) plus the shim's shell; the execs are noise (≈12 ms for
all 18). The +30 MiB session-cgroup delta matches pasta + shim.
