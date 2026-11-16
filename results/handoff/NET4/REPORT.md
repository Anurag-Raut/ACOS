# NET4 — the network module's boot cost, its MCP surface, and one session-naming rule

**Branch** `net4` (from `net3` `5a273439`). **Status: DONE with two measured residuals
(§7).** The relay arm's session boot is no longer 4.7–12.6 s: the cause was *not* the
relay, it was the shared boot-image cache being thrashed between worktrees, and it is
fixed at the root (a content-identity cache key + last-use pruning). `acos_net` is
advertised over MCP with the schema the session API dispatches, a one-shot `verify`
session is addressable by the same naming rule as a `serve` session, and the
documented `assert_text.timeout_ms` wait is now real — which is what made the residual
`net-check` flake a race instead of a mystery.

| # | item | verdict |
|---|---|---|
| 1 | relay-arm boot root-caused and fixed | **PASS** (4.7–12.6 s → 0.28 s median at N=1, 0.28–0.51 s at N=5); residual +130 ms vs module-off, see §7 |
| 2 | `acos_net` advertised over MCP, CLI/MCP parity | **PASS** — `run/serve/serve-net.log` 9/9 |
| 3 | hint text no longer names the non-existent `acos net up` | **PASS** — `acos-netd up` |
| 4 | one-shot `verify` sessions addressable by `acos_net` | **PASS** — `run/session-name/proof.log` |

---

## 1. The relay arm's boot latency: the boot-image cache, not the relay

**What NET3 measured** — `session_start_ms` 4.7 s (N=1) and 9.3–12.6 s (N=5) on the
relay arm versus 0.17–2.2 s with the module off, while attach was 25–65 ms. The
suspicion was the session netns (`--map-host-loopback`, `ip link set lo up`, DNS,
IPv6 RS/DAD, binder traffic at boot).

**What the engine already knew, in the same artifacts.** `engine.json` carries the
boot image's cache outcome (`H13b`). Read across every NET3 run, the split is exact:

| run | `timing.boot_image.cache` | `prep_ms` | `session_start_ms` |
|---|---|---|---|
| `n1` (relay) | **built** | 4049 | 4680 |
| `n1b` (relay) | **built** | 10736 | 12578 |
| `n1c` (relay) | **built** | 3912 | 4223 |
| `n5` (relay) | 1 built / 4 queued | 4151 | 9311–9476 |
| `n5-24g` (relay) | 1 built / 4 queued | 10678 | 11660–11726 |
| `n5-netoff` | all hit | – | 2191 (all five) |
| `n5x3` (relay) | all hit | – | 220–564 |

So the slow runs are the ones that **rebuilt the shared boot image inside the guest's
boot** (or queued behind another session's rebuild under the cache lock); the fast
relay runs of the same day are `cache=hit`. The relay adds ~100 ms; it never added
seconds. That is also why the stage table was empty in NET3: `acos-art`'s `stage()`
markers went into the launcher's stderr *tail*, which the engine discarded (fixed, §6).

**Root cause.** `acos-art`'s shared boot image is cached at
`~/.cache/acos/art/boot-images/fixed-<key>`, and the key hashed the sysroot's
**absolute path** (`inputs()`: `fnv_bytes(&mut hash, root.sysroot…)` plus every input
file's absolute path). Every agent worktree mounts the same image at a different path
(`<wt>/.cache/art/sysroot`), so every worktree produced a *different* key for
*identical* bytes. `KEEP_ENTRIES` is 2 (each entry is ~173 MiB), and pruning was
ordered by build time, so one worktree's build evicted another worktree's entry, whose
next launch then rebuilt it — a thundering herd of 4–12 s `dex2oat` runs on a host that
runs a dozen agents. The live cache directory showed the same image under two keys
(`fixed-26e1ff44c591a5dd`, `fixed-c2ea5a91e85b9251`).

**Fix (class, not symptom), `crates/acos-art/src/boot_image.rs`.**

* `inputs_for_sysroot()` hashes the sysroot's **content identity**, not its location:
  a fixed `b"sysroot"` tag where the path used to be, and every input file by its
  **path relative to the sysroot** (`fnv_file_identity(hash, sysroot, path)`). Same
  image anywhere on the host → one key → one build, shared by every worktree.
  The `ArtRoot`-shaped entry point `inputs()` is now a wrapper, so the key is testable.
* `prune()` orders entries by **last use**, not build time, and a lookup (hit *or*
  build) touches a `.use-<key>` stamp next to the entry. An entry a session is running
  on is never the oldest; entries used within `KEEP_FRESH_SECS` (15 min) survive even
  above `KEEP_ENTRIES`, with `MAX_ENTRIES` (3 × ~173 MiB) as the hard cap so several
  genuinely different images cannot grow the cache without bound. Stamps of vanished
  entries are removed.

**Tests** (`cargo test -p acos-art --lib boot_image`, 15/15):
`the_same_image_at_two_paths_has_one_key` (two mount points, hard-linked inodes → one
key; a rebuilt jar still misses) and
`pruning_keeps_a_fresh_entry_and_evicts_a_stale_one` (a fresh entry survives a third
build; the stale, least-recently-used one is evicted). Both fail on the pre-NET4 code:
the key differed per path, and prune ordered by build time, so the "kept three entries"
assertion could not hold.

**Measured after the fix** (`run/bootn1c/boot.log`, `run/n5relay/boot.log`; every run
`cache=hit`, one key `ff39b037f1d6b9ff` for all of them):

| arm | N | `session_start_ms` (all sessions) | median |
|---|---|---|---|
| relay | 1 | 631, 278, 275, 281 | **280** |
| off | 1 | 156, 152 (hits); 4069, 3831 (`built`, another worktree evicted) | **154** |
| relay | 5 | 325 375 424 458 508 | 424 |
| relay | 3 reps × 5 | rep2: 280–447; rep3: 4088–4221 (one rebuild) | 375 |

**Residual, measured, not hidden:** (a) relay boot is **+130 ms** over module-off at
N=1 (0.28 s vs 0.15 s) — better than 4.7 s but not yet inside the +100 ms target;
(b) a worktree still running a *pre-NET4* binary keeps its path-based key and prunes
by build time, so it can still evict the shared entry (visible as `cache=built` in two
of these runs). Both disappear once the other agents' binaries are rebuilt; (b) is
inherently transitional, (a) is the module's own attach cost and needs its own budget
decision (§7).

## 2. `acos_net` over MCP: one schema, two surfaces

NET3 found the verb working through a live `acos serve` daemon but absent from
`tools/list` (``unknown tool `acos_net` ``). NET4 puts the verb's contract where both
surfaces read it — `crates/acos-netd/src/net_ctl.rs`:

* `NET_CTL_TOOL` — the name, used as the *pattern* in the session API's dispatch arm
  (`acos-cli/src/engine/serve.rs`) and in this server's tool list, so the two cannot
  drift;
* `net_ctl_schema()` — a **closed** (`additionalProperties: false`) JSON Schema with
  `action` (`status|offline|online|delay|hostport`), `ms`, `jitter_ms`, `loss_pct`,
  `port`, `on` and the transport's identity keys;
* `NetAction::from_args()` — the parser the session API now uses, stricter than the
  schema on purpose: an argument that means nothing for the chosen action
  (`{"action":"offline","port":8081}`) is `INVALID_ARGUMENT`, not a silent no-op.

`acos-mcp` gains the `acos-netd` dependency and advertises the schema verbatim
(`descriptors.rs`), with a "what it returns" entry in `help.rs` and the docs table in
`docs/MCP.md` regenerated. No dispatch code was needed: `session_call` already forwards
every non-doctor session tool to the daemon.

**Proof** — `run/serve/serve-net.log` drives the same live `acos serve` daemon twice,
through its own socket protocol and through `acos-mcp`:

```
check=daemon_launch PASS
check=daemon_status_reports_offline PASS session row offline=true after `offline`
check=daemon_delay_reported PASS session row delay_ms=400
check=daemon_hostport_reported PASS host_ports contains 18081
check=mcp_lists_acoustic_tools PASS 20 tools
check=mcp_acos_net_advertised PASS tools/list names acos_net: True
check=mcp_acos_net_schema_closed PASS ["action","base","jitter_ms","loss_pct","ms","on","port","request_id","session_id"]
check=mcp_acos_net_status PASS ok=True action=status networked=True relay_up=True
check=mcp_acos_net_rejects_unknown_argument PASS ... INVALID_ARGUMENT ...
== 9 PASS / 0 FAIL
```

The last two are the parity that matters: the MCP call reaches the *same* verb (the
reply's `networked:true` is the session's own relay row) and the *same* closed schema
(an unknown argument is refused end to end). `crates/acos-mcp/tests/net_ctl_parity.rs`
asserts the descriptor's `inputSchema` **is** `net_ctl_schema()` value-for-value, that
`tool_infos` renders it identically, that every schema key parses and that
non-schema keys do not.

## 3. The hint text

`acos net up` is not a CLI verb (`acos` has no `net` subcommand; the relay's CLI is
`acos-netd`). `serve.rs` now says:

> the network relay supervisor is not running (…); start it with `acos-netd up`

`grep -rn "acos net up" crates/` → no hits.

## 4. One naming rule: a one-shot `verify` session is addressable too

NET3 named the relay session `acos-<id>` for a serve session and `art-<pid>` for a
one-shot run, so `acos_net` could never address the second kind — the session existed
and nothing could name it. NET4 removes the second rule:

* `spike_net::net_session_name(id) -> "acos-<id>"` — the only rule; `SESSION_ID_ENV`
  (`ACOS_ART_SESSION_ID`) carries the id. The launcher's relay arm now **fails closed**
  (`Error::Layout`) when no id was published, instead of inventing `art-<pid>`
  (`acos-art/src/lib.rs`).
* `acos-cli`'s ART engine always publishes an id: the `acos serve` session id, or
  `verify-<pid>` for a one-shot run, and `reset()` keeps the same id (a relaunch is the
  same session for the client that addresses it). `verify` writes it to
  `engine.json` (`engine.session_id`), so a run's network can be named after it exits.

**Proof** — `run/session-name/proof.log` watches the relay's own session table while a
`verify` run holds its session:

```
engine_session_id=verify-2612885
relay_session_names=acos-verify-2612885
check=one_shot_session_is_addressable PASS id=verify-2612885 relay_name=acos-verify-2612885
check=no_art_pid_names PASS
```

## 5. The residual flake was two bugs, not one

1. **The guest hung on an IPv6 address it could not route.** The session interface is
   IPv4-only (`plan`/`pasta_args` configure `10.163.x.x/30`; `LinkProperties` reports
   one IPv4 address), but `dnsproxyd` answered with the host's `getaddrinfo` verbatim:
   `getaddrinfo example.com ^ 1024 0 1 0 0 -> 222 answers=3` (A + AAAA). Android's
   `HttpURLConnection` has no default connect timeout, so when the IPv6 address came
   first the fetch sat in `pending` for the whole 60 s budget and the guest log had no
   completed-fetch line at all. `spike_net::addresses_for_the_session()` drops `AAAA`
   answers while `session_has_ipv6()` is false — netd's resolver answers per-network
   families the same way, and `LinkProperties` already says the session has no IPv6.
   Test: `the_resolver_never_answers_an_address_family_the_session_lacks`.
2. **`assert_text`'s `timeout_ms` was ignored.** The schema documents it as an
   event-driven wait ("the runtime waits for a matching frame/tree, never for a fixed
   sleep") and `scenarios/net-check.yaml` relies on it (`timeout_ms: 60000` on a status
   line a background fetch fills in), but `executor.rs` took a single `observe()` and
   returned in ~1 ms — the same defect W17 fixed for `wait_for`. Now
   `observation_assertion` polls the live tree with the `wait_for` pacing until the
   predicate holds or the budget is spent, then reports the last sample with
   `polls`/`waited_ms` in the failure details; an assertion that names a timeout cites
   the sample that passed instead of the previous step's frame, and an assertion
   *without* one keeps the strict frame binding and one sample. Test:
   `an_assertion_timeout_waits_for_the_tree_and_a_bare_assertion_does_not`.

After both: the fetch completes in **5/5** concurrent relay sessions
(`run/n5relay/relay-rep2/*/logcat.txt`: five `NetCheck: fetch result=HTTP 200`
lines) and `n5relay/relay-rep1` is **5/5 pass, 10/10 steps**. What remains in the N=5
runs is a different, pre-existing class: `TIMEOUT: strict settle timed out
(probe-floor)` on `capture`/`observe` when five guests run at once (§7).

## 6. Boot-stage visibility

`ACOS_ART_STAGE_TIMING=1` produced an empty table on every `verify` run because the
engine drained the launcher's stderr into an 8 KiB tail and dropped it. The drain now
forwards `acos-art: stage …` lines to the CLI's stderr when the variable is set (the
tail still keeps everything). `run/bootn1c/relay-rep4/1.err` carries the 24 markers of
a relay-arm boot (`launch-entry … guest-spawned exec=30ms … guest-exit exec=510ms`),
which is what proves no *in-guest* stage grew: the seconds were spent before the
launcher's own clock, in `prep_ms`.

## 7. What is not done (measured)

1. **+130 ms at N=1** (relay 280 ms median vs module-off 154 ms). Attach is 25–65 ms;
   the rest is the proxy process + netns/veth setup on the spawn path. NET3's target
   was +100 ms; closing the last 30 ms needs a decision (do we pay it per session, or
   pre-create the session netns/veth while the launcher builds its shim?) rather than
   another round of measurement.
2. **N=5 settle timeouts** (`strict settle timed out (probe-floor)`) on
   `capture`/`observe` — pre-existing, appears in NET3's own record, and reproducible
   with the module *off* on this host. It is a settle-budget/`probe-floor` question in
   the ART capture path, not a network one; `relay-rep1` shows 5/5 is reachable.
3. **Cross-agent cache eviction by pre-NET4 binaries** (two `cache=built` runs above):
   transitional; it disappears when the other worktrees rebuild `acos-art`.
4. `acos-netd`'s per-action strictness (a key that means nothing for the chosen action
   is refused) is stricter than the flat JSON Schema can express; the schema documents
   the keys, the parser enforces the pairs, and both are tested.

## 8. Acceptance evidence

| criterion | verdict | evidence |
|---|---|---|
| `cargo test -p acos-netd -p acos-mcp -p acos-cli` green | **PASS** | §9 command output |
| NET3's `proof.log` checks still pass | **PASS** | `run/proof/proof.log`: `hostport_opted_in localhost=200 map=200`, `hostport_not_opted_in_refused`, `other_host_loopback_ports_refused`, `hostport_revoke`, `stale_supervisor_replaced old_pid=2601363 new_pid=2603969`, `guest_cgroup_enforced … python_exit=137`, `no_leak relay_veths=[] session_rules=0 forwarders=0` |
| n5x3 (5/5 × 3) still passes | **PARTIAL** | `run/n5relay/boot.log`: rep1 **5/5** (10/10 steps, boot 325–508 ms), rep2 2/5, rep3 1/5 — the failing runs are `TIMEOUT: strict settle timed out (probe-floor)`, not network (§7.2); the fetch itself succeeds in 5/5 |
| boot timing table before/after under `results/handoff/NET4/` | **PASS** | §1 tables + `run/bootn1c/boot.log`, `run/n5relay/boot.log`, NET3's `engine.json` files |

## 9. Reproduce

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_NET_PASTA=$PWD/.cache/net4/passt-src/pasta   # pinned 2026_10_02.cba3570
export ACOS_NETD_STATE=$PWD/.cache/net4/netd-state       # private supervisor state
export ACOS_RUNTIME_DIR=$PWD/.cache/net4/rt ACOS_SESSION_SOCKET=$PWD/.cache/net4/rt/session.sock
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
CARGO_TARGET_DIR=$PWD/.cache/net4-target tools/guarded.sh --mem 12G --time 900 -- \
  cargo build --release --offline -p acos-netd -p acos-art -p acos-cli -p acos-mcp
ACOS_APP_ONLY=net-check tools/build-apps.sh              # the net-check fixture

# boot table (relay vs module-off, N and reps are arguments)
tools/art-runtime/run-exclusive.sh --task NET4 -- tools/guarded.sh --mem 20G --time 1300 -- \
  bash results/handoff/NET4/boot-driver.sh $PWD/results/handoff/NET4/run/bootn1c 4 1
ARMS="relay" tools/art-runtime/run-exclusive.sh --task NET4 -- tools/guarded.sh --mem 20G --time 1300 -- \
  bash results/handoff/NET4/boot-driver.sh $PWD/results/handoff/NET4/run/n5relay 3 5
# MCP + session naming + NET3's host-port proof
python3 results/handoff/NET4/serve-net.py $PWD/results/handoff/NET4/run/serve
bash results/handoff/NET4/proof-session-name.sh $PWD/results/handoff/NET4/run/session-name
BIN=$PWD/.cache/net4-target/release/acos-netd bash results/handoff/NET3/proof-hostport.sh \
  $PWD/results/handoff/NET4/run/proof
```

## 10. Environment / cleanliness

`pasta` was rebuilt from the pinned tag (`sha256 314c37e8…0edf60`, identical to
NET1–NET3) into `.cache/net4/passt-src`; no downloads beyond that source tree, no sudo,
no kernel change. Private `ACOS_NETD_STATE`/`ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET`
for every run; nothing outside the worktree except the ART runs' own scratch. Disk 77 %
→ 79 % and back; the build target is deleted at the end of the turn. Peak RAM during
the N=5 runs: five ART guests inside a 20 GiB guard (host under ~22 GiB used).
