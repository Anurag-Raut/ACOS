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
