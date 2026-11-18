# NETREAL — real-HTTPS network proof (v0.4.0 exit item)

Pinned `pasta` 2026_10_02.cba3570 (sha256 314c37e8…edf60, see
`research/spike-net/artifacts/passt-build.txt`) found by the product's own rule at
`<checkout>/libexec/pasta` (gitignored; an install carries it at
`<prefix>/share/acos/<version>/libexec/pasta`). No `ACOS_NET_PASTA` /
`ACOS_SPIKE_NET*` / `ACOS_NET_ARM` set anywhere. Re-run: `bash results/handoff/NETREAL/proofs.sh <outdir>`.

| arm | scenario | result |
| --- | --- | --- |
| off | net-network-off | pass 8/8 |
| check-off | net-check-offline | pass 9/9 |
| check | net-check (network:real) | pass 10/10 |
| real | net-network-real | pass 9/9 |
| offline | net-network-offline (onLost, bounded failure, onAvailable, recovery) | pass 14/14 (5/5 reps after the fix) |
| https-proof | net-https-proof: HttpsURLConnection GET https://example.com via relay, `tls=ok cipher=TLS_AES_128_GCM_SHA256`, leaf CN=example.com, HTTP 200, hostname-verification negative `tlsneg=rejected SSLHandshakeException` | pass 12/12 |
| host-port | NET3 proof-hostport.sh | 8/8 checks PASS, no leak |

Evidence: `run/final/` (proof.log, per-arm verdicts, logcat, hostport), `run/tests/` (cargo test logs:
core crates rc=0; `spike_net_boundary` 2/2 with pasta installed and nothing set, and
`test-spike-net-boundary-nopasta.log`: both FAIL loudly naming the missing pinned pasta).
The request sent to example.com is a bare GET; nothing personal.

## Bugs found and fixed
1. `net-https-proof.yaml`: the ordered `assert_log` listed `fetch result` before `tls`; the app logs `tls` first. Scenario bug.
2. `LinkProperties` of a networked session did not encode (bare values where a counted element is required) and panicked the launcher at the first network callback. Fixed in `connectivity.rs` (+ byte round-trip tests; `RouteInfo`'s `IpPrefix` is a named parcelable).
3. `net-network-offline.yaml`: un-timed `assert_log` for `netcb onLost/onAvailable` raced the callback (failed once in six runs); now `timeout_ms: 10000` (event-driven).
4. `proof-hostport.sh`: `guest_cgroup_enforced` missed an OOM because the scope lives ~80 ms (added the user-manager journal as witness); `no_leak` read `session_rules` before the supervisor's 500 ms reaper tick (now waits up to 5 s).
5. `net-check.yaml` had no `services:` block (it depended on `ACOS_SPIKE_NET=1`, which no runner sets) so with pasta present it ran the off arm.
6. `install.sh` did not ship `acos-netd`, so an install could not attach `network:real` at all.

## Not changed / notes
`spike_net_boundary` skip-vs-fail is HYG2's `require_prereq`: missing pasta fails unless `ACOS_ALLOW_TEST_SKIP=1`.
