# SPIKE-NET — report (ADR-0030 spike gate: real network through pasta)

> **Session 3 update (2026-11-16).** The app-level gate is **GO**: a real HTTPS GET of
> `https://example.com` returns `HTTP 200` on screen through the session's namespace,
> bionic's `dnsproxyd` proxy and the image's CA store, and the same APK fails honestly
> with the flag off (`SecurityException`, no VALIDATED capability). `LinkProperties` is
> at device parity (interface, address, DNS server, default route, DHCP server). The
> **A/B gate was not produced**: `tools/bench` is broken at this commit (and on `main`)
> — three defects fixed here, two open in other tracks — so no M3/M4/M5/M6/M7/M8, M2 PSS
> or N=20 cell exists for either arm. **Recommended default: `off`**, now on measured
> grounds rather than "no data": the boundary costs ~**30 MiB per session** (pasta
> 27.6 MiB RSS + the shim shell; the 18 `iptables` execs are ~12 ms and the after-dump
> ~2.4 ms), on a 74–109 MiB session. Session-3 verdicts are in
> [§11](#11-session-3-verdicts-app-level-proof-parity-and-the-ab), the transcript in
> `research/spike-net/LOG.md` §"Session 3"; evidence under
> `results/handoff/SPIKE-NET/net-check/`.

Branch `t3/spike-net` (worktree `/home/Qui/.t3/worktrees/acos/t3-2f28c6b2-bee01c021cecb8a6`).
All spike code is gated on `ACOS_SPIKE_NET=1`; with the variable unset the sandbox flags,
the seccomp program, the session startup and the connectivity capabilities are the
previous ones (the spike only adds code behind the gate). Transcript of every attempt:
`research/spike-net/LOG.md`.

**Overall: PARTIAL — transport and namespace design GO, framework/app proof and the A/B
gate NOT DONE, so the recommended default is `off`.**

| # | Item | Verdict | Evidence |
|---|---|---|---|
| 1 | pasta built static from official source, version + sha256 + licence recorded | **GO** | `research/spike-net/artifacts/passt-build.txt` |
| 2 | pasta attached to the session netns; host loopback and LAN unreachable, internet reachable | **GO, with two mandatory changes to ADR-0030** | `research/spike-net/artifacts/probe-hardened.txt` |
| 3 | seccomp allows `AF_INET`/`AF_INET6` only with the flag | **GO (implemented, default program unchanged)** | `crates/acos-art/src/seccomp.rs` |
| 4 | dnsproxyd (netd replacement) | **PARTIAL** — protocol identified and a responder implemented; no in-guest proof | `artifacts/bionic-getaddrinfo-excerpt.txt`, `crates/acos-art/src/spike_net.rs` |
| 5 | connectivity reports a validated INTERNET network with the flag | **PARTIAL** — `NET_CAPABILITY_VALIDATED` added; `LinkProperties` still empty; not proven in-app | `crates/acos-services/src/connectivity.rs` |
| 6 | test app: HTTPS GET via `HttpURLConnection`, status on screen | **NOT DONE** | — |
| 7 | A/B metrics (cold, warm, memory, N=20 density) vs flag off | **NOT DONE** | — |
| 8 | default recommendation strictly from the A/B gate | **default `off`** (no A/B evidence; owner's condition unmet) | — |

## 1. pasta (item 1)

Official source, pinned release tag `2026_10_02.cba3570`
(commit `cba357068dc586e7540971eb40bb56c5e62c4de1`, 2026-09-28), `make static`:

* version banner `pasta 2026_10_02.cba3570`, 1,548,232 bytes, statically linked x86-64
* sha256 `314c37e878bd6ffe43200735800d5efd4859d81d46b0450358a103c36c0edf60`
* licence GPL-2.0-or-later (`LICENSES/GPL-2.0-or-later.txt`) plus BSD-3-Clause for the
  bundled compat/siphash files — ADR-0030's "GPL-2 shipped with its licence" is satisfied
  and needs no system package.
* `passt` and `pasta` are one binary; the release build also emits `passt.avx2`, which the
  binary prefers and falls back from with a one-line warning when absent (the installer
  should ship both or accept the warning).

## 2. Namespace, isolation and what the guest can reach (item 2) — GO with changes

pasta is attached by running bwrap **inside** the netns pasta creates
(`pasta --config-net … -- /bin/sh -c '<LAN guard>; exec "$@"' sh bwrap …`), which is the
only unprivileged option here: bwrap cannot join an existing netns, and a plain parent
cannot `setns()` into a netns owned by a child user namespace. With that, a full ART
session boots to first frame with the flag on — pasta's userns, bwrap's nested userns and
the application uid 10192 compose correctly (`artifacts/art-boot-pasta.txt`, wall 5172 ms
for `hello-views`).

Measured from inside the namespace (`artifacts/probe-hardened.txt`):

```
public tcp:    connect 104.20.23.154:443 -> OK
host loopback: connect 127.0.0.1:18080   -> Connection refused   (host had a listener there)
lan gateway:   connect 192.168.31.1:80   -> Connection timed out
udp dns:       1.1.1.1:53 -> OK
RESULT internet=reachable host_loopback=blocked lan=blocked dns=ok
```

**Two findings the ADR must absorb — pasta's defaults are not safe for ACOS:**

1. **Host loopback leak by default.** `-T/--tcp-ns` and `-U/--udp-ns` default to `auto`:
   pasta mirrors every port the host has bound on loopback into the guest namespace and
   splices local traffic between the two L4 stacks. With plain `pasta --config-net` the
   guest connected to the host's `127.0.0.1:18080` and could *list* the host's loopback
   listeners (`ss -ltn` showed `*%lo:53`, `*%lo:22`, `*%lo:18080`, …). ACOS must always
   pass `-T none -U none --no-splice` (plus `--map-host-loopback none`, implied
   `--no-map-gw`). ADR-0030 §3 currently names only `--no-map-gw`, which is **not
   sufficient**.
2. **The LAN is reachable through pasta's NAT and there is no destination ACL.**
   `192.168.31.1:80` answered until RFC1918/link-local DROP rules were installed *inside*
   the session netns. The netns is owned by pasta's user namespace, so the command pasta
   spawns is root **in that netns** with `CAP_NET_ADMIN` there and nowhere else, which is
   enough for `iptables -A OUTPUT -d 192.168.0.0/16 -j DROP` (and 10/8, 172.16/12,
   169.254/16). This works today but it is a kernel-netfilter dependency in the guest
   path (nft/iptables availability, and `iptables` inside a userns-owned netns must keep
   working) — the ADR should record it as the LAN-block mechanism, not a footnote.

Also from the hardened run: the guest gets a link-local address (`169.254.2.15/24`,
gateway 169.254.2.1) rather than the host's LAN address, so nothing in the guest looks
like it is on the LAN; DNS is handed out as `1.1.1.1` via DHCP.

**Security summary (what a guest can reach with the hardened invocation):** the public
internet on any TCP/UDP port (that is the feature), DNS to 1.1.1.1 via pasta's NAT, and
nothing else: no host loopback (refused), no RFC1918/link-local destination (dropped), no
inbound connection (no `-t/-u` forwards; `-T/-U none`), no route to the host's own
addresses. Residual risks, all of which need the ADR's fresh Opus review: the guest shares
the host's public egress IP; pasta itself is a fairly large C surface running **outside**
the sandbox for every session; the `iptables` chain lives in a namespace the guest
controls (it can `iptables -F` — so the guard is a *policy*, not a boundary against a
malicious app inside the guest); and the DNS responder answers from the host resolver,
which is a deliberate information channel out of the guest.

## 3. seccomp (item 3)

`build_program_with(action, allow_inet)`; with `allow_inet == false` the emitted program is
instruction-for-instruction the previous one, and `build_blob_with()` passes
`spike_net::enabled()`. `AF_INET`/`AF_INET6` become reachable only when the flag is on.
A full ART boot with the flag on exercises the new branch end to end (item 2's evidence);
an isolated "socket(AF_INET) succeeds with the flag and fails without" unit test was not
written — that is the smallest gap in this item.

## 4. DNS (item 4) — PARTIAL

The AOSP cache is a sparse bionic tree (no `libc/dns/`), so the resolver source was taken
from googlesource at `android-14.0.0_r1`. The protocol is **text, one request per
connection**, on `/dev/socket/dnsproxyd`:

```
getaddrinfo <host|^> <serv|^> <flags> <family> <socktype> <protocol> <netid>\0
-> "222\0", then per address: BE32 1, BE32 flags/family/socktype/protocol,
   BE32 addrlen, sockaddr, BE32 namelen, name; terminated by BE32 0
```

(excerpt in `artifacts/bionic-getaddrinfo-excerpt.txt`; `gethostbyname` in the same file
also goes through this proxy).

Implemented: `acos_art::spike_net::serve_dnsproxyd`, started for a session only when the
flag is on, bound at `<session socket dir>/dnsproxyd` — the directory the sandbox already
binds over `/dev/socket`, so the guest finds it at the device path. It answers
`getaddrinfo` with the host's `libc::getaddrinfo` and answers `gethostbyname` with a
structured error (netd has a separate command for it; bionic's `gethostbyname` does not
appear to need it, but this is unproven).

**Not proven:** no guest process was observed completing a resolution through it. That is
the single most valuable next test (an APK that resolves `example.com`, or an ART-side
`InetAddress.getByName` fixture).

**CA store:** the image already ships `/system/etc/security/cacerts` (134 hashes) and
`cacerts_google` — nothing to add for TLS. (Note: the AOSP CA store is the image's, so
`HttpURLConnection` trust for public CAs should work as-is; not tested.)

## 5. Connectivity (item 5) — PARTIAL

`default_capabilities_bits()` adds `NET_CAPABILITY_VALIDATED` only with the flag, so
`isConnected`/`isInternetReachable` both report true (the pair ADR-0030 §5 asks for).
Still missing for parity with a device: `LinkProperties` addresses/DNS servers, and
`WifiManager`/`TrafficStats` consistency. Not proven against an app's
`ConnectivityManager` callback in this spike.

## 6. App-level proof (item 6) — NOT DONE

No fixture APK performs a request; building and driving one (new `fixtures/apps/net-check`
+ `tools/build-apps.sh` + a verify run) did not fit the 60-minute budget. What *is* proven
is everything under it: the namespace, the transport, the seccomp gate and a booting ART
session inside it.

## 7. A/B metrics (item 7) — NOT DONE, therefore default `off`

No numbers were produced for cold launch, warm launch, per-session memory or the N=20
density check, flag off vs flag on. The only timing observed is a single flag-on cold
launch (5172 ms, `hello-views`, under the shared lock, `--exclusive` not used) which is
**not** a valid A/B datum and must not be quoted as one.

Per the owner's condition (`real` is the default only if the tracked metrics do not worsen
beyond noise), the recommendation is:

> **Default `off`** for v0.4.0. The network module ships as `off` + `real` (attach on
> demand) until the A/B is run on the merged implementation: cold launch, warm launch,
> per-session memory and N=20 density, interleaved reps, `tools/bench/ab-compare.py` gate,
> on a quiet host. Two costs are already predictable and belong in that measurement:
> one `pasta` process per session (~a few MiB RSS, plus a `/bin/sh` and `iptables` exec at
> session start), and the DNS responder thread inside the launcher.

## 8. Remaining work, in order

1. Run the A/B (item 7) on this branch: `tools/bench/` with `ACOS_SPIKE_NET=0` vs `=1`,
   interleaved, `--exclusive`, N=20 density via `tools/guarded.sh`; gate with
   `tools/bench/ab-compare.py`. This is the only thing that can flip the default to `on`.
2. In-app HTTPS proof (item 6) — new fixture `net-check` (HttpURLConnection GET of
   `https://example.com`, status code into a `TextView`), plus a resolution proof for item
   4 in the same run.
3. Turn the spike into the module: pasta discovery + version check in `acos doctor`,
   packaging/licence install, `iptables` guard owned by the module (not a `/bin/sh`
   wrapper), DNS responder as a service with `gethostbyname` and netd `DnsResolver`
   binder stub, `LinkProperties` addresses/DNS, then the hot controls
   (offline/online, latency/loss, HAR proxy).
4. ADR-0030 amendment for the two findings above (`-T none -U none --no-splice`; LAN block
   via in-namespace netfilter; note that pasta runs outside the sandbox and that the
   guard is policy, not a boundary against guest code).

## 9. Orchestrator decisions needed

* Approve the ADR-0030 amendment in §2 (pasta flags + netfilter) — the current text's
  `--no-map-gw`-only phrasing is measurably insufficient.
* Accept `default off` for v0.4.0 pending the A/B, or schedule the A/B as the first task
  of the network track.


---

## 10. Session 2 verdicts

Branch `t3/spike-net-session-2-gate` (base `4638d9f4`, session 1's work). Everything is
still gated on `ACOS_SPIKE_NET=1`; with the flag unset the sandbox flags, the seccomp
program and the connectivity bits are the previous ones.

| # | Item | Verdict | Evidence |
|---|---|---|---|
| 1 | drop rules installed before the guest, and the guest cannot change them | **GO** | `results/handoff/SPIKE-NET/boundary/test-run.log`, `boundary/net-boundary.txt`, `boundary/guest-probe.stdout`; `crates/acos-art/tests/spike_net_boundary.rs` |
| 2 | `fixtures/apps/net-check` + scenario (items 4/5/6 at app level) | **PARTIAL — fixture built, run not executed** | `fixtures/apps/net-check/`, `scenarios/net-check.yaml`, `fixtures/build/apps/net-check.apk` |
| 3 | A/B gate (cold, warm, PSS, N=20 density), `ab-compare.py` rule | **NOT DONE** | — |
| 4 | default recommendation strictly from the A/B | **`off`** (no A/B numbers ⇒ no evidence `on` is free) | this section |

### 10.1 The boundary (item 1) — GO

`spike_net::boundary_script()` replaces the old `/bin/sh -c '<guard>; exec "$@"'` string:

* netfilter chain `acos-net-drop` (RFC1918, `169.254/16`, `100.64/10`) jumped from
  `OUTPUT`, **verified rule by rule** before bwrap starts;
* **fail closed**: no `iptables`, a chain that cannot be created or a rule that does not
  verify ⇒ exit `111`, guest never started (proven by
  `boundary_failure_never_starts_the_guest`: `boundary=failed reason=no-iptables exit=111`,
  empty probe stdout);
* the shim dumps the chain before *and* after the guest; a change would be a diff.

Measured from inside the real guest mount view (`boundary/guest-probe.stdout`):

```
probe: uid 10192
probe: cap-eff 0000000000000000                      no capability at all
probe: userns 4026533637   shim/netns owner 4026533566   a *child* user namespace
probe: socket-netlink DENIED:EACCES                  AF_NETLINK blocked by seccomp
probe: so-mark DENIED:EPERM                          no CAP_NET_ADMIN over the netns
probe: socket-raw-inet DENIED:EPERM
probe: iptables-flush exit=1
probe: iptables-flush-output iptables: Failed to initialize nft: Permission denied
probe: connect-public OK                             the feature still works
probe: connect-lan Operation now in progress         DROPped (connect times out)
probe: connect-host-loopback Connection refused      `-T none --no-splice`
```

The two chain dumps in `boundary/net-boundary.txt` are identical (`rules=installed` /
`rules=after … guest-exit=0`), and the host read `/proc/<guest>/status` while the guest ran:
`CapEff=0` and the guest's userns differs from the netns owner's. `SO_MARK` is the
capability probe because the kernel checks it against `sock_net(sk)->user_ns` — the very
right needed to rewrite the chain.

### 10.2 The seccomp `AF_INET` branch was dead code — fixed here

Session 1's `allow_inet` branch was emitted *after* `jeq(AF_UNIX, list, deny)`, so every
non-`AF_UNIX` family hit the deny terminal first: with the flag on, `socket(AF_INET)`
returned `EACCES`. Session 1's probe ran inside the netns but outside bwrap, and an ART
boot reaches first frame without creating its own socket, so nothing noticed.

Fixed to fall through `AF_UNIX → AF_INET → AF_INET6` into the syscall deny list.
`seccomp::tests::socket_families_follow_the_spike_flag` interprets the emitted cBPF
program: `AF_INET`/`AF_INET6` are allowed only with the flag, `AF_NETLINK` is denied
either way, and the flag-off program is unchanged. That test fails on session 1's code.

### 10.3 Fixture and app-level proof (items 4/5/6) — PARTIAL

`fixtures/apps/net-check` (`HttpURLConnection` GET of `https://example.com`; `id=status`
holds `HTTP 200` or the exception class name; `id=connectivity` holds the
`ConnectivityManager` report) and `scenarios/net-check.yaml` (asserts `HTTP 200`,
`internet=true validated=true`, and the `NetCheck` log line) exist and the APK builds
(`ACOS_APP_ONLY=net-check tools/build-apps.sh`).

**Not run:** `ACOS_SPIKE_NET=1 acos verify --engine art` on it. The release `acos` was
built (1 m 12 s) but an ART verify run writes 0.5–2 GB of artifacts
(`results/handoff/ARTGC.md`) and the host disk crossed the gate before it could start.
So item 4 (dnsproxyd) and item 5 (`LinkProperties`) keep session 1's PARTIAL status, and
item 6 has a fixture but no measurement.

### 10.4 A/B gate (item 3) — NOT DONE, default stays `off`

No interleaved reps, no cold/warm launch, no PSS, no N=20 density; nothing is quoted as an
A/B number. Per the owner's condition the recommendation is unchanged:

> **Default `off`** for v0.4.0. `real` is available only if the A/B
> (`tools/bench/acos-bench.sh` off vs on, interleaved, `tools/bench/ab-compare.py` gate)
> later shows no metric worse than +5 %/+2 ms (fast metrics) and nothing outside the old
> gate's band.

Costs that measurement will have to absorb, now measured in structure rather than time:
one `pasta` process per session, one `/bin/sh` shim that stays resident for the session's
lifetime (needed for the after-dump), 5 + 1 `iptables` invocations at session start (each
is an `xtables-nft-multi` exec), one DNS responder thread in the launcher.

### 10.5 Remaining work, smallest first

1. `ACOS_SPIKE_NET=1 acos verify --engine art --apk fixtures/build/apps/net-check.apk
   --scenario scenarios/net-check.yaml` and the same with the flag off (must show
   `UnknownHostException`/`SocketException`) — closes items 4/5/6.
2. `LinkProperties` addresses/DNS/routes in `acos-services::connectivity` (item 5 parity).
3. The A/B gate, on a host under the disk gate.

---

## 11. Session 3 verdicts (app-level proof, parity, and the A/B)

Branch `t3/spike-net-session-2-gate`, base `9cac941f`, commits `d05ce3cb` (app-level
proof + parity) and the report commit. Everything still gated on `ACOS_SPIKE_NET=1`;
with the flag unset the sandbox flags, the seccomp program, the connectivity bits and
the `LinkProperties` bytes are the previous ones.

| # | Item | Verdict | Evidence |
|---|---|---|---|
| 1 | pasta pinned, static, licence recorded (session 1) | **GO** | `research/spike-net/artifacts/passt-build.txt` (re-built this session: same sha256 `314c37e8…`) |
| 2 | namespace: internet reachable, host loopback and LAN unreachable; the LAN block is a boundary | **GO** | §2, §10, `results/handoff/SPIKE-NET/boundary/`, `crates/acos-art/tests/spike_net_boundary.rs` |
| 3 | seccomp allows `AF_INET`/`AF_INET6` only with the flag | **GO** | `seccomp::tests::socket_families_follow_the_spike_flag` (fails on session 1's code) |
| 4 | dnsproxyd (netd replacement): the guest's resolver reaches it and gets answers | **GO** | §11.1; `results/handoff/SPIKE-NET/net-check/on/dnsproxyd-requests.log`; `dnsproxyd_socket_is_world_writable_and_answers` |
| 5 | connectivity reports a validated INTERNET network **and** a real `LinkProperties` | **GO** | §11.2; `net-check/{on,off}/verdict.json`, `logcat.txt`; 3 `link_properties_tests` |
| 6 | in-app HTTPS GET of `https://example.com` renders `HTTP 200`; flag off fails honestly | **GO** | §11.1/§11.3; `net-check/on/3-net-check-ok.png`, `net-check/off/3-net-check-offline.png` |
| 7 | A/B (cold, warm, PSS, N=20 density) vs flag off, `ab-compare.py` gate | **BLOCKED — the bench harness is broken at this commit; only a lean substitute A/B exists** | §11.4/§11.5 |
| 8 | default recommendation strictly from the A/B | **`off`** — no gate was produced, and the per-session memory cost measured outside the bench (+30 MiB) is far outside any noise band | §11.6 |

### 11.1 Item 4/6 — the app-level proof, and the wall that was in the way

`ACOS_SPIKE_NET=1 acos verify --engine art --apk fixtures/build/apps/net-check.apk
--scenario scenarios/net-check.yaml` → **`pass` (10 steps)**:

```
NetCheck: connectivity active=100 internet=true validated=true iface=wlan0 addrs=1 dns=1 dnsList=1.1.1.1 routes=1
NetCheck: dnsprobe dnsprobe exists=true canRead=true canWrite=true …
NetCheck: body-first-bytes-len=256
NetCheck: fetch result=HTTP 200
dnsproxyd: req="getaddrinfo example.com ^ 1024 0 1 0 0" -> 222 answers=3
```

The wall, found in the guest's own words: `/dev/socket/dnsproxyd` did not exist for the
app. Four different directories are bound at `/dev/socket`, and the verify/live path uses
`launch.rs`'s (`<plan>/<tag>/sock` under the production zygote), not the session
launcher's `sock_dir` session 1 had served. bionic's `android_getaddrinfo_proxy`
therefore failed to `connect()` and answered `EAI_NODATA` — the app showed exactly
`UnknownHostException: … No address associated with hostname`. `serve_dnsproxyd` now
returns a stoppable handle and is started in **every** `/dev/socket` producer
(`launch.rs`, `session.rs`, `app.rs`), `0666` like the device's socket. TLS needed
nothing: the image's CA store trusts `example.com` as-is.

### 11.2 Item 5 — `LinkProperties` parity

`describeNetwork()` in the fixture now prints the DNS server values, and the scenario
asserts `iface=wlan0 addrs=1 dns=1 dnsList=1.1.1.1 routes=1` on top of
`internet=true validated=true`. Reported values are the namespace's real ones:
`169.254.2.15/24`, DNS `1.1.1.1`, default route `0.0.0.0/0 via 169.254.2.1 dev wlan0`,
DHCP server `169.254.2.1`, MTU 1500, `wlan0`. They are constants in
`acos-services::connectivity` and `spike_net::pasta_args()` builds `-a/-g/-D` from them,
so the interface pasta configures and the one apps read cannot drift (a test asserts it).

Codec work this needed (not a stub): `FieldOp::AddressList` (int count +
`writeByteArray` per address, IPv6 scope-id form included) and specs for
`android.net.LinkAddress`, `android.net.RouteInfo` and `android.net.IpPrefix`; the
`LinkProperties` spec's three `Int(0)` counts became variable-length lists. Flag-off
bytes are unchanged (an empty list writes the same `0`), and
`flag_off_keeps_every_list_empty` pins it.

### 11.3 Item 6, flag off — the honest failure

`env -u ACOS_SPIKE_NET acos verify --engine art --scenario scenarios/net-check-offline.yaml`
→ **`pass` (9 steps)**, i.e. the failure is asserted, not tolerated:

```
connectivity … internet=true validated=false iface=wlan0 addrs=0 dns=0 dnsList=none routes=0
dnsprobe exists=false …
fetch failed: java.lang.SecurityException: Permission denied (missing INTERNET permission?)
fetch result=java.lang.SecurityException
```

Session 2 predicted `UnknownHostException`/`SocketException`. The measured class is
`java.lang.SecurityException`: the seccomp filter denies `socket(AF_INET)` with `EACCES`
and libcore's `IoBridge` maps that to `SecurityException(… missing INTERNET permission?)`
before any name is resolved. The offline scenario accepts either class and the logcat
records which one happened. No fake success, no VALIDATED bit, no socket.

### 11.4 Item 7 — the A/B gate was not produced: `tools/bench` is broken

Run exactly as documented (`acos-bench.sh --no-build --reps 1 --latency-reps 10
--fast-reps 16 --task SPIKE-NET`, own `--target-dir`/`--scratch-root`, `ACOS_ART_BIN` and
the arm's env exported; the driver is `research/spike-net/ab/run-arm.sh`), every phase fails **before a number is produced**, on the off
arm as well as the on arm:

| defect | location | state |
|---|---|---|
| `NameError: _environ_has` | `d2_warm.py` `survivors()` | **fixed here** (Python twin of `tools/bench/reap-daemons.sh::bench_environ_has`) |
| `NameError: _record_daemon` | `d2_warm.py` `Serve.__init__` | **fixed here** (appends `pid starttime` to `ACOS_BENCH_PIDFILE`) |
| `UnboundLocalError: cohort` | `acos_bench.py` `phase_loop` | **fixed here** (initialised before the `try`; it masked the real error) |
| `RuntimeError: two consecutive settled captures differ: [None, None]` | `phase_latency` | **open** — 4.8 s failure on the off arm too |
| `RuntimeError: reference tree is not the count-0 screen: count='0' nodes=14` | `phase_cold` | **open** — stored reference tree no longer matches the rendering |

These are not this spike's regressions: `git diff main HEAD -- tools/bench` is empty, so
`main` (`a9d351b2`) fails identically, and nothing has run the bench since the
zygote-owner commits. Closing the last two means re-baselining a reference tree and
redefining "settled" — rel3/PERF-REL3 decisions, and re-baselining to make a gate pass is
exactly what the rules forbid.

### 11.5 What could still be measured (substitute, clearly not M3/M4/M4-PSS)

Interleaved, same binary, same scenario, alternating arms, `acos verify --engine art` on
`hello-views` (`research/spike-net/ab/lean-ab.sh`, 2 reps/arm; per-run artifacts
   deleted):

| measurement | flag off | flag on |
|---|---|---|
| launch step wall (`steps[0].wall_ms`) | 155, 155 ms | 154, 155 ms |
| ART session boot (`session_start_ms`) | 177, 177 ms (one cold boot-image build: 3894 ms) | 176, 219 ms |
| session cgroup **peak**, `hello-views` | 108, 109 MiB | 144, 147 MiB |
| session cgroup **peak**, `net-check` | 74 MiB | 104 MiB |

Per-session cost of the boundary itself (direct measurement, not inference):

| part | cost |
|---|---|
| `pasta` process (production flags, re-execs into `passt.avx2`) | **27.6 MiB RSS**, 1 thread |
| the shim's `/bin/sh` (resident for the session: it writes the after-dump) | ~4 MiB RSS |
| chain install **and** rule-by-rule verification (1 `-N`, 5 `-A`, `OUTPUT` jump, 5 `-C`, 1 `-C`) | **7 ms** |
| the before+after dumps = 4 of the 18 `iptables` execs | **≈2.4 ms** (10 × `iptables -S` = 6 ms) |
| pasta + shim end to end (netns + userns + chain + 2 dumps) | **31 / 36 / 42 ms** wall |

So the shim's after-dump is **not** what costs: the cost is `pasta` (~28 MiB RSS, one
process per session) plus its shell, and the execs are noise. The +30 MiB cgroup delta is
exactly pasta + shim.

### 11.6 Default recommendation — `off`

Strictly from the A/B the answer is unchanged from sessions 1–2: **no gate was produced,
so `on` is not proven free**. This session adds the reason to keep it `off` even before
the bench works: an extra `pasta` process per session costs **~28 MiB RSS resident plus
~4 MiB of shim shell** (measured session delta +30 MiB on a 74–109 MiB session, i.e.
+28 % to +40 %), while its wall cost is ~31–42 ms at session start with no detectable
change in the launch step (155 vs 154/155 ms). A +30 MiB per-session regression is
outside any noise band the gate can express.

> **Default `off`** for v0.4.0: `real` is attach-on-demand until (a) the bench runs the
> four tracked metrics again, and (b) the per-session cost is either accepted by the
> owner as a module cost or reduced (one pasta per *session* is the design; a shared
> pasta relay would change ADR-0030 §3 and needs its own security review).

### 11.7 Remaining work, smallest first

1. Repair the bench (`phase_latency` settle capture, `phase_cold` reference) on its own
   track, then run item 7 for real (`--reps 3`, interleaved, `--exclusive`,
   `ab-compare.py`). The three harness defects fixed here are already in this branch.
2. If the owner wants `real` as the default despite the memory cost: make the shim's
   after-dump optional (it is 4 of 18 execs, ≈2.4 ms — a diagnostic, not a boundary) and
   re-measure; it will not recover the 28 MiB.
3. Productise: pasta discovery/version in `acos doctor`, packaging, the `iptables` guard
   owned by the module (not a shell shim), `DnsProxy` as a service with
   `gethostbyname` + netd `DnsResolver` binder, then the hot controls (offline/online,
   latency/loss, HAR proxy).

### 11.8 One residual finding, unrelated to the gate

`android.net.LocalSocketImpl.connectLocal` has **no native implementation** in this
image (the fixture's raw-socket probe reports `UnsatisfiedLinkError: No implementation
found for void android.net.LocalSocketImpl.connectLocal(…)`). Nothing in the gate depends
on it (bionic's resolver is C, not `LocalSocket`), but any app using `LocalSocket` on
ACOS will fail. Not fixed here — it is an image/libcore gap, not a network one.
