# NET6 — the network goes away and comes back, the way Android's does

**Branch** `net6` (from `modules` `78e9c955`). **Status: DONE.** NET5's open
finding — after `offline`, a second fetch in the same guest process never
returned — is diagnosed, fixed at four causes, and proven on real ART in one
process. `scenarios/net-network-offline.yaml` is a gate now (14/14).

## 1. The diagnosis (NET5's "smallest next action", done)

One ART run with a stuck-fetch watchdog in the fixture put the frame on record:
the second fetch was a **reused keep-alive TLS socket** blocked in
`SocketInputStream.socketRead0` under `Http1xStream.readResponseHeaders`. The
relay dropped the packets, nothing answered, nothing timed out — and the
resolver was never asked because Java's `InetAddress` cache answered in 1 ms.
The full stack and the supporting `/proc`/`ss` evidence are in
`FINDING-RESOLVED.md`.

Diagnosing *why the app could not even observe the loss* found three wire
defects on the host → app path (a VAL_PARCELABLE without its length prefix, an
extra `Message` uid word the image does not have, and `CALLBACK_LOST` sent as
`CALLBACK_LOSING`'s code). Each is measured, not guessed; details in the same
file.

## 2. The fix

| area | change |
|---|---|
| relay (`acos-netd`) | `offline` = the packet `DROP` **plus**, inside the session netns, `ip route del default` and `ss -K` of established/syn-sent/close-wait TCP sockets; `online` restores the route then removes the rule. `plan::session_offline_cmds` / `session_online_cmds`, applied by `Supervisor::set_offline`. |
| resolver (`acos-art`) | the `dnsproxyd` responder answers `500` (EAI_NODATA) while the session is offline. |
| framework (`acos-services`) | `ConnectivityState` gains `default_up`: `getActiveNetwork()`/capabilities/link properties/info answer the empty/`null` Android answers while offline; `set_default_network(up)` sends one `CALLBACK_LOST` per live request (down) or the four registration messages (up). The `network` module's `offline`/`online` control drives it and queues the deliveries on the host. |
| wire (`acos-services`) | `write_parcelable_bundle` uses `write_dynamic` (VAL_PARCELABLE with its length prefix); `encode_message` writes the image's single uid word; `CALLBACK_LOST = 4` with `CALLBACK_LOSING = 3` documented. |
| fixture (`net-check`) | `registerDefaultNetworkCallback` with a cumulative `netcb` line; a stuck-fetch watchdog that writes every Java stack to `/data/app-data/.../diag.txt` (logcat stops carrying the app's lines during the old hang); a `retry` that resolves then fetches. |

## 3. Proof on ART (one build, one process)

`results/handoff/NET6/run/proof/proof.log`:

```
relay_start rc=0 "pasta_version": "2026_10_02.cba3570"
arm=off     exit=0 verdict=pass steps=8/8    scenarios/net-network-off.yaml
arm=real    exit=0 verdict=pass steps=9/9    scenarios/net-network-real.yaml
arm=offline exit=0 verdict=pass steps=14/14  scenarios/net-network-offline.yaml
hostport_runs exit=0 check=no_leak "relay_veths": [] "session_rules": 0 forwarders=0
```

The offline arm, from the app's own log
(`run/proof/offline/art/logs/logcat.txt`):

```
NetCheck: netcb onAvailable 100
NetCheck: fetch result=HTTP 200
NetCheck: netcb onLost 100
NetCheck: fetch failed: java.net.ConnectException: Failed to connect to example.com/172.66.147.243:443
NetCheck: fetch result=java.net.ConnectException          (1.03 s after the tap)
NetCheck: netcb onAvailable 100
NetCheck: fetch result=HTTP 200
```

The scenario asserts, in order: the first fetch is `HTTP 200`; the app is
listening (`netcb onAvailable`); `service_ctl offline`; `netcb onLost`; the
retry's status becomes an exception within 30 s (measured 1.0 s) and the log
names an Android-shaped one (`UnknownHostException` / `ConnectException` /
`SocketException` / `NoRouteToHostException`); `service_ctl online`;
`netcb onAvailable`; the same retry is `HTTP 200`; no crash.

The callback assertions use the app's log line rather than the `netcb` TextView:
a `runOnUiThread` update posted from a binder thread does not wake ACOS's idle
main looper promptly (measured; see `FINDING-RESOLVED.md`, residual). The
callback itself is delivered — the log line is the app's own `onLost`.

## 4. Acceptance

* `cargo test -p acos-netd -p acos-services` — **PASS**, `rc=0`, 66 green test
  binaries (`run/acceptance-tests.log`). Two wire tests moved with the fix:
  `net1_connectivity` now reads a bundle value's length prefix and asserts a
  callback Message ends exactly after `sendingUid` (the image's layout).
* The `a11y` fixture APK that `moda11y6_gate` needs is not in the worktree's
  `fixtures/build` after the prescribed `cp -al` setup; rebuilt with
  `ACOS_APP_ONLY=a11y-probe tools/build-apps.sh` (environment, not code).

## 5. What changed for `offline` semantics (worth knowing downstream)

`offline` now has a *guest-visible* half. A test that asserts the relay's
`offline: true` row still passes, and a session that is offline can no longer
resolve names or open connections — which is the point. The relay's `ss -K`
needs `ss` (iproute2, already required for `ip`/`iptables`/`tc`).

## 6. Reproduce

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_NET_PASTA=$PWD/.cache/net6/passt-src/pasta       # pinned 2026_10_02.cba3570
export ACOS_SPIKE_NET_PASTA=$ACOS_NET_PASTA
export ACOS_NETD_STATE=$PWD/.cache/net6/netd-state ACOS_RUNTIME_DIR=$PWD/.cache/net6/rt
export ACOS_SESSION_SOCKET=$ACOS_RUNTIME_DIR/session.sock
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
CARGO_TARGET_DIR=$PWD/.cache/net6-target tools/guarded.sh --mem 14G --time 1800 -- \
  cargo build --release --offline -p acos-netd -p acos-art -p acos-cli
ACOS_APP_ONLY=net-check tools/build-apps.sh
bash results/handoff/NET6/proofs.sh $PWD/results/handoff/NET6/run/proof
```

`pasta` was rebuilt from the pinned tag into `.cache/net6/passt-src` (the
worktree's network cache was empty); `status` reports
`"pasta_version": "2026_10_02.cba3570"` and the supervisor's tag check passed.

## 7. Not done / for the orchestrator

1. **Looper wake on a binder-thread post** (residual above). A host-originated
   callback is delivered, but an app that updates its UI from the callback sees
   the update only when the next input wakes the main looper. It is a runtime
   (looper/host-pump) question, not a network one, and it is why the scenario
   asserts callbacks on the app's log.
2. The `+101 ms` N=5 attach delta stays accepted per the orchestrator's decision.
