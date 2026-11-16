# SPIKE-NET — report (ADR-0030 spike gate: real network through pasta)

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
