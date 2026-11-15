# VIEW1b handoff — the live view hub (`acos view`, consumer side)

**Worktree:** `/home/Qui/Projects/acos/.cache/wt-view1`
**Branch:** `live-view-2`, from `live-view-1` = `0adcbe0`, merged with `main`
(`74e4ee9` = v0.2.0) as `cef6d9f`. Not pushed, not merged.
**Track:** VIEW1b (live view phase 2, consumer). Owner decision: ADR-0027.
**Producer contract:** `docs/LIVE_VIEW.md` §1–§7 (VIEW1a, unchanged except §8).

---

## 1. What exists now

`acos view` — the local browser hub the owner asked for: 127.0.0.1 only, random
token in the URL, works in T3 Code's preview pane, view-only, frames only while
a viewer is connected.

```
$ acos view
acos view: http://127.0.0.1:54321/?t=<token>
```

* **One self-contained page** (inline CSS/JS, ~15 KB, no CDN, no external
  request, nothing served from disk): a grid of live sessions from the registry
  (agent/client, project dir, APK/package, current activity, age, display,
  watchers) that refreshes every 2 s; clicking a session opens its live screen
  with tap ripples at the event coordinates, the current activity and the recent
  actions (tool, masked text, status). `--json` prints the same one line as a
  JSON record; `--once [--json]` lists the sessions and exits.
* **Security:** binds exactly `127.0.0.1` (`--bind 0.0.0.0` / `::` / a hostname
  exit 2 before a socket exists); a fresh 32-byte (256-bit) token from
  `/dev/urandom` is required on **every** route (`/`, `/api/sessions`,
  `/api/frames/<id>`, `/api/events/<id>`) and compared in constant time
  (missing/wrong → `403`); `GET` only (`405`); no request shape forwards input to
  a guest; no path parameter touches the filesystem (a session id is only matched
  against the registry).
* **One upstream subscription per session, only while watched.** The first
  browser connection for a session connects to its `stream_socket` and sends the
  one subscribe line (`v 1`, png, scale 0.5, 15 fps); later connections join it
  (fan-out, one encode per frame). The last connection to leave makes the hub
  close the socket and **join the upstream thread before returning**, so the
  producer's subscriber count is back at zero. A slow browser stalls at most one
  2 s write and is dropped; the newest frame wins, so it skips frames instead of
  blocking the producer. A closed tab is noticed by an EOF poll on the idle
  stream (not by waiting for the next frame, which never comes on a static
  screen).
* **Registry only:** stale entries (dead `daemon_pid`) are hidden from the list
  and swept from disk; a `stream_socket: null` entry is listed as metadata only;
  a restarted producer is reconnected automatically.

### Files

| file | change |
|---|---|
| `crates/acos-cli/src/view/mod.rs` | **new** (~700 lines with tests): CLI args, 127.0.0.1 bind check, token generation + constant-time compare, minimal std-only HTTP/1.1 server (GET + query token), MJPEG frame stream, SSE event stream, EOF-poll of an idle browser, page/JSON routes |
| `crates/acos-cli/src/view/hub.rs` | **new** (~700 lines with tests): registry discovery + stale sweep, one shared upstream per session (protocol v1 client: subscribe line, `H`/`F`/`E` framing, 64 MiB cap, interruptible reads), latest-frame slot, 128-event ring, watcher refcount whose last detach joins the thread |
| `crates/acos-cli/src/view/page.rs` | **new**: the page (grid, session view, ripples, activity, action list) |
| `crates/acos-cli/src/main.rs` | `mod view;` + `Command::View` |
| `crates/acos-art/src/live_view.rs` | `ACOS_VIEW_STATS_FILE` also writes **once** when the last subscriber leaves (dev-only seam, so a test can show the count back at 0; no cost with no viewer) |
| `docs/LIVE_VIEW.md` | §8: the hub documented (command, URL/token, endpoints, fan-out, view-only, T3 Code, cost) |
| `docs/AGENT_GUIDE.md` | the limits bullet now says how to watch a session (`acos view`, T3 Code preview pane) |
| `docs/MCP.md` | §1c: how an agent/the owner opens it |

No new crate, no new dependency: the workspace already had `serde_json` and
`libc`; the HTTP/1.1 server is ~400 lines of `std::net` (no tokio/hyper in the
workspace, and the dispatch forbids new network crates).

---

## 2. Acceptance

Every ART run, gate and `cargo test` went through
`/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task VIEW1b --
/home/Qui/Projects/acos/tools/guarded.sh --mem 10G --time 900 --`. Builds went
through `tools/guarded.sh --mem 14G --vmem 40000000 --time 1500 --`.

### A. Unit tests — **PASS** (8 new, real sockets, real HTTP)

`cargo test --release --offline -p acos-cli view::` → **8 passed, 0 failed**.

| test | asserts |
|---|---|
| `the_token_is_required_on_every_route` | `/`, `/api/sessions`, `/api/frames/x`, `/api/events/x` without a token → **403**; a wrong token → 403; with the token the page is 200 self-contained HTML and `/api/sessions` is 200 JSON; unknown path with a token → 404; `POST` → 405 |
| `the_token_comparison_is_constant_time_and_length_safe` | exact match only; shorter/longer/case-different tokens fail; a random token is 64 hex chars (256 bits) and two calls differ |
| `only_loopback_may_be_bound` | `127.0.0.1` ok; `0.0.0.0`, `::`, `::1`, `192.168.1.10`, `localhost`, `""` refused with "127.0.0.1 only" |
| `the_listing_hides_stale_sessions_and_reports_metadata` | a live entry (real pid) is listed with client/project/package/activity/display/`stream:true`/`age_ms`/`watchers`; a dead-pid entry is hidden **and its file swept**; a `stream_socket: null` entry lists with `stream:false` |
| `the_upstream_socket_closes_when_the_last_browser_leaves` (hub) | two watchers → one upstream connection and the producer's socket stays open when only one leaves; the last detach closes it (fake producer sees its subscriber count go 1 → 0) and a later watcher makes exactly **one new** connection (no leak) |
| `one_upstream_subscription_is_fanned_out_to_every_browser` (hub) | 5 watchers → `accepts == 1`; every watcher receives the same frame and action event; a late watcher joins the same upstream |
| `the_listing_hides_and_sweeps_stale_entries` (hub) | same sweep through the hub API, plus `watch()` refuses a metadata-only entry |
| `frames_and_events_stream_and_close_the_upstream_with_the_last_browser` (HTTP, fake protocol-v1 producer) | 2 MJPEG responses + 1 SSE response share **one** upstream connection; a multipart part is `Content-Type: image/png` and carries the PNG bytes; SSE sends `state` (with the entry and the `hello`) and `ev` for the tap at `x=540`; dropping all three connections takes the producer back to 0 subscribers, `watchers 0`, `connected false`, `upstream_connects 1` |

### B. Live — **PASS (16/16 checks)** with a real counter session

Harness: `.cache/view1b-proof/drive.py` (not committed; evidence in
`.cache/view1b-proof/run/report.json`). A real `acos-mcp` (its own private
`acos serve --engine art`, clientInfo `view1b-proof 1.0`) launched
`counter.apk`; the registry was redirected with `ACOS_VIEW_REGISTRY_DIR` into
the scratch dir; then the driver did exactly what the page does.

| # | check | result |
|---|---|---|
| 1 | `mcp_initialize` | ok |
| 2 | `launch` (art, pass) | ok |
| 3 | `once_json_lists_session` (`acos view --once --json`) | ok, 1 session |
| 4 | `prints_one_line_url` (`http://127.0.0.1:<port>/?t=<64 hex>`) | ok (text form verified separately: `acos view: http://127.0.0.1:<port>/?t=<token>`) |
| 5 | `no_token_is_403` (no token and a wrong token) | ok, both 403 |
| 6 | `page_is_self_contained` (15,129 B, no CDN, no `<script src>`, no stylesheet link) | ok |
| 7 | `sessions_listed` (package `dev.acos.fixtures.counter`, client `view1b-proof 1.0`, activity `…MainActivity`, display 1080×2400, `stream:true`, `watchers:0`) | ok |
| 8 | `frames_content_type` (`multipart/x-mixed-replace; boundary=acosframe`) | ok |
| 9 | `taps_accepted` (2 taps on "Increment") | ok |
| 10 | `producer_sees_one_subscriber_while_watched` (stats seam: `subscribers: 1`) | ok |
| 11 | `frames_decode` — **26 parts, 4 distinct encoded, 3 distinct decoded PNGs**, every part a PNG, 520,804 B total | ok |
| 12 | `sse_state_and_tap_events` — `state` (entry + hello `{v:1, display 1080×2400, format png, scale 0.5}`) and **2 tap events** at `x 157, y 431, status "ok"` | ok |
| 13 | `hub_reports_zero_watchers_after_close` (`watchers:0`, `connected:false`) | ok |
| 14 | `destroy` | ok |
| 15 | `producer_back_to_zero_subscribers` — final stats `subscribers: 0`, `subscribers_dropped: 1` | ok |
| 16 | `session_disappears_after_destroy` (registry empty, `/api/sessions` empty) | ok |

The subscriber-count proof, precisely: while the browser watched, the producer's
own seam said `subscribers: 1` (26 encodes, 26 frames sent, 0 dropped-latest,
1.26 ms and 20.0 KB per frame). After the tab closed, one more action was
delivered at the guest: a still-open hub socket would have received that event,
instead the producer's send failed and its count went to **0** (dropped 1), which
the seam wrote out on the transition. Same seam VIEW1a used; no test seam was
weakened (the only producer change is that transition write).

### C. Gate metrics — **NOT RUN (host disk ceiling)**

The dispatch's A/B (viewer OFF, M4–M8 parity against a base build of
`live-view-1`) was **not run**. `/` sat at **85 % (135 GB free) at the start and
at the end** — the host directive says prune at 85 % and not to add large
artefacts — and a base arm in its own linked worktree needs a second ~7 GB
target dir, which would put the disk at ~87 %.

What is known instead, structurally and honestly:

* `acos view` is a **separate process**. The benchmark never starts it, so
  viewer-off cannot differ from VIEW1a's measurement unless the producer
  changed.
* The producer diff against `live-view-1` is **one block in `writer_loop`**
  (`crates/acos-art/src/live_view.rs`): the stats-file write, which is skipped
  entirely unless `ACOS_VIEW_STATS_FILE` is set (the bench does not set it).
  `git diff 0adcbe0 -- crates/acos-art` is that block plus nothing else. No
  command-path code, no tap thread, no capture, no encode, no allocation.
* VIEW1a already measured viewer-off parity for this producer (every M4–M8
  delta inside ±1.5 %, M7-fast inside the 2 ms floor), so the claim to re-prove
  is "a viewer-OFF session still costs what live-view-1 cost". That needs the
  A/B run above, not a code argument, and is listed as remaining.

### D. Gates — **PASS** (release build, `--engine art`, `.cache/view1b-gates/`)

| scenario | status | fidelity |
|---|---|---|
| `v2-art-counter` | pass | art-reference |
| `form-validation` | pass | art-reference |
| `gaten-home` | pass | art-reference |

Each verdict also reports `artifact_validation.ok: true` and
`certification: certified-with-limits` (the standard sandbox certification).

### E. Tests — **PASS**

`cargo test --release --offline -p acos-cli -p acos-art -p acos-mcp
--no-fail-fast` → **455 passed, 0 failed, 5 ignored** (the pre-existing
`#[ignore]`s), 2 m 22 s under the lock.

---

## 3. Decisions

1. **MJPEG for frames, SSE for events, two connections per browser.** MJPEG is
   the browser-native way to stream PNGs (no base64, no decoder in JS) and SSE
   carries the JSON feed natively (`EventSource` reconnects by itself). The hub
   refcounts *connections*, not tabs: a tab holds one of each, and the upstream
   closes when both are gone.
2. **A closed tab is detected by `recv(MSG_PEEK|MSG_DONTWAIT) == 0`, not by
   waiting for the next message.** A static screen sends no frames, so
   "close when the module closes" would otherwise leave the subscription (and
   the producer's encode path, if the screen ever moved) alive. The EOF poll is
   free and runs at most twice a second per browser.
3. **The hub is the only subscriber; the page never connects to a session
   socket.** That is what makes fan-out real (one encode for N viewers) and
   keeps the browser from ever holding a producer socket.
4. **`--bind` refuses everything but `127.0.0.1`, at argument-parse time.** A
   loopback-only listener plus a token are both required by ADR-0027; refusing
   before `bind(2)` makes the guarantee testable without sockets.
5. **Token from `/dev/urandom`, compared byte-by-byte over the expected length,
   never logged.** It is printed once in the single startup line and lives in the
   page URL only (the page reads `location.search`), so an agent can hand the URL
   to T3 Code with no other setup.
6. **The registry is the only filesystem input,** and a session id never becomes
   a path: the id from the URL is matched against the registry map, and only the
   registry's own `stream_socket` is connected.
7. **The stats seam now writes once on the last detach.** Without it, a session
   that is destroyed while static never writes a "0 subscribers" snapshot (the
   writer only refreshes while someone watches, and the launcher's teardown
   write is not guaranteed), so the acceptance claim "the subscriber count is
   back to 0" could not be shown. Dev-only, no viewer → the block is not
   entered.
8. **`XDG_RUNTIME_DIR` is not redirected in the live proof.** The dispatch
   suggested a scratch `XDG_RUNTIME_DIR`, but `acos-art` needs the user D-Bus to
   create its memory-cgroup scope, and a scratch value makes every launch fail
   with `MEMORY_CGROUP_UNAVAILABLE`. The registry — the only new path this track
   adds — is redirected with `ACOS_VIEW_REGISTRY_DIR` into
   `.cache/view1b-proof/run/registry`; the ephemeral session/stream socket lives
   in the normal `/run/user/1000/acos*/` dirs, exactly as in VIEW1a, never in
   `~/.local`.

---

## 4. Remains / next actions

1. **The viewer-off A/B** (§2C). Smallest next action: free ~12 GB (prune another
   track's stale target dir, or `git worktree remove` an old one), then
   `git worktree add .cache/wt-view1-base 0adcbe0`, build it with its own
   `CARGO_TARGET_DIR`, check the compile paths with `strings`, and run the
   interleaved `tools/bench/acos-bench.sh --reps 1 --groups "a:latency:8G:300"`
   2 reps per arm (ACOS_BENCH_TASK=VIEW1b), then delete the base worktree.
2. **Viewer-ON cost of the hub** is unmeasured: the hub adds a thread per
   browser and a 20 KB write per frame; the producer's own encoder cost is
   unchanged (1.26 ms / 20 KB per frame in this run, matching VIEW1a).
3. Polish not required by the acceptance, in priority order if it is ever
   wanted: a `--format jpeg` switch for the hub's subscribe line (the producer
   already supports it, and jpeg is smaller on image-heavy screens), a
   `/api/sessions` long-poll instead of the 2 s poll, and a per-session
   activity badge in the grid (it is already in the JSON).
4. `docs/LIVE_VIEW.md` §8 and the two guides now describe the hub; if the
   endpoints change, keep §8 and the page in step (the page is the only client).

---

## 5. Resources

* **Disk:** `/` was at 84 % → **85 %** across this track (149 GB → 135 GB free;
  no new large artefacts were created: the target dir is VIEW1a's
  `.cache/view1-target`, 7.0 GB). Witness files: `.cache/view1b-proof/`
  (180 KB: `drive.py`, `report.json`, 6 decoded frames), `.cache/view1b-gates/`
  (gate JSON + artifacts), `.cache/lv/` (unit test scratch, safe to delete).
  No base worktree was created (see §2C), so nothing to delete there.
* **Host RAM:** 60 GiB; `MemAvailable` sampled at 31–32 GiB before and after;
  the guards (`--mem 10G` for ART runs, `--mem 14G` for builds) never tripped;
  no OOM, no swap (0 B). No run held the exclusive lock longer than ~6 minutes.
* **Left running:** nothing. The private `acos-mcp`/`acos serve`/`acos-art` of
  the proof and the `acos view` process were all terminated by the driver; no
  session registry entry remains.

---

## §Review — VIEW1 security review findings (Opus review, as given in the VIEW1c brief)

* **M1** `view.sock` was bound in `sockets.0`, which is bind-mounted rw into the guest as `/dev/socket`: the APK could subscribe (read unmasked typed text), evict the hub (`MAX_SUBSCRIBERS` drops the oldest), exhaust host threads (unbounded accept threads + byte-wise 5 s reads), replace the socket (spoof frames/events), or race `set_permissions` through a symlink.
* **L1** typed text/key codes unmasked when focus state is unknown or stale; `FLAG_SECURE` windows streamed; password echo undocumented.
* **L2** no `Host` check (DNS rebinding). **L3** no hub connection cap / head deadline. **L4** attach/detach race in the hub. **L5** frames larger than `SO_SNDBUF` always dropped the subscriber. **L6** `/dev/shm` fallback and registry dirs not 0700/owner-checked.

## §VIEW1c — fixes (branch `live-view-2`, checkpoint f0184e06 + docs/evidence commit)

Started from a previous agent's uncommitted ~510-line draft (it did not compile: `struct Inner` unclosed). Kept the right parts (M1 relocation, `private_dir`, `FLAG_SECURE` plumbing, subscribe cap, per-subscriber write offset), fixed the rest.

| finding | fix | tests |
|---|---|---|
| M1 | `view.sock` now in `acos-view-<pid>-<seq>/` (0700, ours, verified; never mounted; guest mounts only `acos-art-*` at `/dev/socket`). Socket 0600 via `fchmodat(dirfd, name, AT_SYMLINK_NOFOLLOW)` — **not** the draft's process `umask`, which raced with other threads (made test dirs 0600) and is gone. Symlink/non-socket at the path refused. Subscribe threads capped at 8 (extras closed at accept), one 5 s total deadline for the line, `SO_PEERCRED` uid check in producer (`subscribe`) and hub (`verify_peer_uid` on upstream connect) | `the_view_socket_is_private_…`, `peer_credentials_report_our_own_uid`, `subscribe_threads_are_capped_and_a_trickling_peer_is_cut_off_by_one_deadline`, `the_view_directory_is_not_the_guest_mounted_socket_directory`, `private_dir_tightens_ours_and_refuses_a_file_or_a_symlink`, hub `the_hub_reads_the_peer_uid_…` |
| L1 | text **and** key codes masked by default; unmasked only when this command's reply carries a fresh tree with a focused non-password node (`tree_proves_non_password_focus`; a tree with no focus masks; staleness cleared per input command). `FLAG_SECURE`: `LayerPlacement.secure` from WMS `LayoutParams.flags`, `SurfaceState::has_secure_window()`; source emits an all-zero 1/8 placeholder with header `"secure":true`; `tap_once` zeroes secure frames again (defence in depth). Password-echo caveat documented in `docs/LIVE_VIEW.md` §3 | `typed_input_is_masked_unless_a_fresh_tree_proves_a_non_password_focus`, `a_secure_frame_is_published_blank_and_flagged`, services `a_visible_flag_secure_window_is_reported_…` |
| L2 | `Host` must be exactly `127.0.0.1:<port>` else 403 before any route | `a_host_other_than_the_loopback_literal_and_port_is_refused` |
| L3 | hub cap 64 connections (closed at accept), 5 s total head deadline | `connections_are_capped_and_a_stalled_head_is_cut_off_by_one_deadline` |
| L4 | slot keeps its socket; after the last detach's join, if `watchers > 0` the upstream is restarted | `a_browser_that_attaches_while_the_last_one_detaches_still_gets_a_stream` |
| L5 | per-subscriber pending buffer + offset, drop only on error or >8 MiB queued; writer flushes every 1 ms while queued | `a_frame_larger_than_the_socket_buffer_is_delivered_to_a_slow_reader` (2.9 MB frame, subscriber kept) |
| L6 | registry dir created 0700 via `DirBuilder`, owner/dir/symlink verified, loose own dir tightened; reader refuses a dir another uid owns or that is group/world-writable; `/dev/shm` fallback goes through the same `private_dir` | `the_registry_directory_is_private_owned_and_never_a_symlink` |

Note: the slow-subscriber drop test now waits for the 8 MiB cap (loop 4000 ticks). A viewer slower than frame production is still dropped once 8 MiB is queued (a deliberate bound, `MAX_QUEUED_BYTES`).

### Acceptance

* **Unit tests:** `cargo test --release --offline --no-fail-fast -p acos-cli -p acos-art -p acos-mcp -p acos-services` → all green, 0 failed (`.cache/view1c-tests.log`).
* **Live proof** (counter via MCP private daemon, `ACOS_VIEW_REGISTRY_DIR` redirected; `results/handoff/VIEW1c-evidence/live-proof-report.json`, driver `drive.py`): **verdict pass, 20/20 checks** — session socket `/run/user/1000/acos-view-<pid>-1/view.sock` dir 0700 / sock 0600; none in this session's `acos-art-<pid>-0` dir; guest inspection through `/proc/<guest pid>/root`: `/dev/socket` = `[logdw, peer.sock]`, host socket path not present, no `view.sock` found under guest `/dev,/data,/run,/tmp`; Host check live (200 / 403 / 403); 27 MJPEG parts, 3 distinct decoded PNGs; tap events; producer subscribers 1 while watched, **0 after** (`subscribers_dropped ≥ 1`); session leaves the registry on destroy.
* **A/B viewer OFF** (`tools/bench/acos-bench.sh --reps 1 --groups "a:latency:8G:300"`, ACOS_BENCH_TASK=VIEW1c, interleaved base,cand,base,cand… actual order base1, cand1, cand2, base2; base built in its own linked worktree, compile paths checked 0 foreign): vs main 477e99c, M4/M6/M7/M8-strict/teardown within noise; **M5 screenshot 11.9→24.3 ms and M8 fast +14…36 % are NOT from VIEW1**: the branch merged main only at 74e4ee9 and lacks main's SHOTZ (M5 24→12, `results/bench/SHOTZ-evidence`) and PERF-REL3 changes (and the brief forbids merging main). Proof: the branch's own pre-VIEW1c commit 1221dfe vs this candidate (2 reps each) is **all within noise** (M5 24.04 vs 24.26, M8 fast 46.4 vs 47.5 / 44.7 vs 44.4, M4 fast 159.3 vs 159.2) — `ab-vs-pre-1221dfe.txt`. Both arms 2 reps; the pre arm ran after the main arm, not strictly interleaved with the candidate. **Re-measure against main after the orchestrator merges main.**
* **Gates** (`--engine art`, `.cache/view1c-gates/`): `v2-art-counter`, `form-validation`, `gaten-home` → pass, fidelity art-reference.

### Remains / decisions for the orchestrator
1. Merge main (not done, per brief) and re-run the viewer-off A/B vs main — the M5/M8-fast deltas should vanish.
2. `launch.rs` also got a `cargo fmt` pass (≈+100 lines of formatting noise in the diff).
3. Stale `/run/user/1000/acos-art-*/view.sock` dirs from earlier killed sessions exist on the host (old layout); harmless, not cleaned.
4. Hub does not forward the frame header's `secure` flag to the page (the placeholder is simply black).

## §VIEW1d — security re-check follow-ups (branch `live-view-2`)

| item | fix | test |
|---|---|---|
| 1 | `observe_request` resets the remembered focus state for `type_text`/`key` **before** the no-subscriber return; also reset when the first viewer attaches | `a_non_password_state_does_not_survive_a_detach` (fails without the fix: a key code leaked after detach) |
| 2 | `LaunchMain.appendViewJson` fallback (no `AccessibilityNodeInfo`): TextView password from `getInputType()` variations (TEXT password/web/visible, NUMBER password) or `PasswordTransformationMethod`, as AOSP `TextView`; if unreadable the node is `password_unknown` (+ `password:true`) and Rust treats a focused `password_unknown` node as password | `a_fallback_shaped_tree_masks_a_password_or_undecidable_focus`; live proof below |
| 3 | `write_atomic` propagates `set_permissions` error; `read_entries` comment states the real rule (owner = us, not group/world-writable; 0700 and 0755 ok); `ShortSocketDir` drop removes the host-only view dir even under `ACOS_ZYGOTE_SESSION` | existing registry/launch tests |
| 4 | `docs/LIVE_VIEW.md` + `docs/AGENT_GUIDE.md`: agent screenshots ignore `FLAG_SECURE`; only the stream blanks | — |

Live proof: `drive.py` extended with a password phase (a scratch `text-input` rebuilt with `inputType=textPassword`, `.cache/view1d-root`, not a committed fixture): `live-proof-report-view1d.json` **22/22 checks pass**; typed text -> `[masked]`, secret absent from the raw SSE, key code -> `[masked]`.
Observation (not changed): the tree attached to a reply is probed *after* the command, so a key that moves focus (ENTER in a single-line field moves focus to the next control) can make a post-command tree prove a non-password focus and leave that one key code unmasked. No typed secret leaks (text is masked from the field that had focus when typed only if the reply tree says so); worth a follow-up decision.
