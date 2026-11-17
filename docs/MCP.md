# ACOS MCP server

`acos-mcp` is a [Model Context Protocol](https://modelcontextprotocol.io) (MCP)
stdio server that exposes the ACOS agent API to AI coding agents. It turns every
ACOS command into an MCP tool, so an agent can call `acos_inspect`,
`acos_verify`, `acos_run`, `acos_observe`, ... directly instead of shelling out
to the `acos` CLI.

The server is deliberately thin: it does not re-implement the runtime. It
adapts the typed contract in [`crates/acos-protocol`](../crates/acos-protocol)
and dispatches to the `acos` binary.

`tools/list` advertises **only what this build can run**. The protocol still
reserves every command name (the descriptors in `acos_protocol::mcp_tools()` are
unchanged, and the table below lists them all), but a command with no
implementation on any route — `acos_capture`, `acos_assert`, `acos_events`,
`acos_profile`, `acos_capability_report` — is hidden: calling one still returns
the same structured `unsupported` envelope with
`error.details.available = false`, it is simply not advertised to an agent that
would otherwise try it. (`acos_run`, `acos_observe`, `acos_action`, `acos_reset`
and `acos_destroy` **are** advertised: the in-process session backend implements
them even though the `acos` CLI has no such subcommand.)

| | |
|---|---|
| Binary | `target/release/acos-mcp` (`cargo build --release -p acos-mcp`) |
| Transport | MCP stdio: newline-delimited JSON-RPC 2.0 on stdin/stdout |
| Protocol revision | `2025-03-26` by default; also accepts `2025-06-18` |
| stdout | protocol frames only |
| stderr | logs (`ACOS_MCP_QUIET=1` silences them) |
| Source | `crates/acos-mcp/` |
| Tests | `crates/acos-mcp/tests/`, `cargo test -p acos-mcp` |

---

## 1. Register the server

The server needs to know where the `acos` binary and the workspace are. Pass
them as environment variables in the client config.

### One command

```bash
tools/release/register-mcp.sh                     # install at ~/.local, user scope
tools/release/register-mcp.sh --prefix <prefix>   # an install elsewhere
tools/release/register-mcp.sh --dry-run           # prints the exact claude command + JSON
```

The script registers the server with Claude Code (`claude mcp add --scope user
-e ACOS_BIN=… -e ACOS_ALLOWED_ROOTS=… -- <acos-mcp>`), resolves both binaries to
absolute paths, prints the equivalent `mcpServers` JSON for every other client,
and registers nothing when `--dry-run` is given. It is idempotent: an existing
`acos` entry at that scope is removed before the new one is added, so a re-run
repairs stale paths instead of duplicating them.

**Installed ACOS wins.** When `<prefix>/bin/acos` and `<prefix>/bin/acos-mcp`
exist (default prefix: `$ACOS_PREFIX`, else `~/.local`; `--prefix` to point
elsewhere) the script registers those **wrappers**, not a checkout build. The
wrappers export `ACOS_HOME` pinned to the versioned install root, so the server
and every `acos` it spawns read the installed tree from any directory and never
touch a checkout. Only when no install is present does it fall back to
`<checkout>/target/release/{acos,acos-mcp}` (then `PATH`). `tools/release/
install.sh --register-mcp` calls it for you after installing.

It defaults `ACOS_ALLOWED_ROOTS` to `$HOME` — see below for why.

### Claude Code, by hand

```bash
claude mcp add --scope user acos \
  --env ACOS_BIN="$PWD/target/release/acos" \
  --env ACOS_ALLOWED_ROOTS="$HOME" \
  -- "$PWD/target/release/acos-mcp"
```

Use `claude mcp list` to confirm it is connected and `claude mcp remove acos` to
remove it.

### `ACOS_ALLOWED_ROOTS`: what value an agent in another repo needs

Every path argument (`apk`, `scenario`, `artifacts`, `path`, …) must live under
one of the roots in `ACOS_ALLOWED_ROOTS` (a `:`-separated list):

* the path is resolved **and** canonicalised, so a **symlink that escapes the
  roots is rejected**, even when the link itself is inside one;
* `..` components are rejected before resolution;
* anything else — a relative path that climbs out, an absolute path in another
  repo, a link into `/etc` — fails with **`UNSAFE_PATH`** (`error.hint` names
  the path and the roots; nothing is read and no guest starts);
* **a relative path is refused before any of that** (FB5): the server's cwd is
  not your shell's, so `apk`, `scenario` and `artifacts` must be absolute. A
  relative one returns **`INVALID_ARGUMENT`** with
  `details.resolved_path` = the absolute path the server *would* have used and
  `details.server_cwd`, plus a hint. Pass an absolute path.

`register-mcp.sh` defaults the value to **`$HOME`** so an agent working in
another repository (for example `~/Projects/gaten`) can pass that repo's APK and
its own `scenarios/*.yaml` while still being unable to reach outside the home
directory. The artifacts directory (`ACOS_ARTIFACTS_DIR`) is always allowed in
addition. On a shared or multi-user host, list the specific repositories
instead (`ACOS_ALLOWED_ROOTS=/srv/agent-a:/srv/agent-b`).

**If you get `UNSAFE_PATH`,** pick one:

1. move (or copy) the APK/scenario under an allowed root — the usual fix, since
   build outputs often live under the repo you are already allowed to read;
2. or add that repository to `ACOS_ALLOWED_ROOTS` in the client's server config
   (colon-separated) and restart the server: with Claude Code,
   `claude mcp remove --scope user acos` and re-run `register-mcp.sh
   --allowed-roots <roots>`;
3. keep produced evidence under the artifacts directory instead of the repo.

A symlinked *root* is fine: the roots themselves are canonicalised at startup, so
`ACOS_ALLOWED_ROOTS=$HOME` accepts a path under a repository reached through a
link inside `$HOME`.

### Generic JSON config

Most MCP clients accept an `mcpServers` object (Claude Desktop, Cursor, Cline,
Continue, ...). Point `command` at the built binary:

```json
{
  "mcpServers": {
    "acos": {
      "command": "/home/you/Projects/acos/target/release/acos-mcp",
      "args": [],
      "env": {
        "ACOS_BIN": "/home/you/Projects/acos/target/release/acos",
        "ACOS_ALLOWED_ROOTS": "/home/you/Projects/acos",
        "ACOS_ARTIFACTS_DIR": "/home/you/Projects/acos/out"
      }
    }
  }
}
```

### Environment variables

Backend selection:

| variable | meaning |
|---|---|
| `ACOS_BIN` | path to the `acos` binary (default: `acos` on `PATH`, else `target/{release,debug}/acos` walking up from the cwd) |
| `ACOS_ALLOWED_ROOTS` | `:`-separated path roots that path arguments may live under (default: the process cwd, plus the artifacts dir) |
| `ACOS_ARTIFACTS_DIR` | directory that holds produced artifacts (default `<cwd>/.acos-mcp-artifacts`). `acos_verify` writes `out/<scenario>/` inside it, bounded by retention (below) |
| `ACOS_KEEP_ARTIFACTS` | artifact retention for runs started through this server: `all`, `referenced` (default) or `none-on-pass` (see `docs/AGENT_API.md` §10.2). A non-pass run always keeps everything |

Server limits (all optional):

| variable | default | meaning |
|---|---|---|
| `ACOS_MCP_MAX_REQUEST_BYTES` | `4194304` | maximum size of one request frame |
| `ACOS_MCP_MAX_CONCURRENCY` | `4` | maximum concurrent backend calls |
| `ACOS_MCP_QUEUE_TIMEOUT_MS` | `5000` | how long a tool call waits for a slot |
| `ACOS_MCP_CALL_TIMEOUT_MS` | `120000` | default call budget when the request declares none |
| `ACOS_MCP_MAX_OUTPUT_BYTES` | `16777216` | maximum captured bytes per child stream |
| `ACOS_MCP_MAX_IMAGE_BYTES` | `524288` | maximum bytes of the image inlined as MCP image content (a preview that would exceed it is downscaled further, then dropped to a link) |
| `ACOS_MCP_PREVIEW_MAX_SIDE` | `1280` | long edge in pixels of an inlined image preview |
| `ACOS_MCP_MAX_TEXT` | `4000` | maximum bytes for one string in a tool reply. A longer string (an error message carrying a 200-frame java tail and a 400-line native tail) is head/tail-capped with a marker naming its original size and the artifact path, and an identical repeat is replaced by a back-reference, so the full text appears once. The **whole reply** is also capped at 20 KiB (`acos_protocol::reply::MAX_REPLY_BYTES`, see §1c); a tree is compacted/paged, and the full evidence is named by a path. `0` disables both bounds (the raw CLI behaviour). The guide text (`acos_help`) is exempt |
| `ACOS_MCP_SESSION_TTL_MS` | `600000` | idle session TTL in **milliseconds**; the FB5 fallback when `ACOS_SESSION_IDLE_TTL_S` is unset |
| `ACOS_SESSION_IDLE_TTL_S` | `600` | FB5: idle TTL in **seconds** for the sessions this MCP server launched (any later call renews it). `0` disables. `acos_launch {idle_ttl_s}` overrides it per session |
| `ACOS_MCP_SHUTDOWN_GRACE_MS` | `2000` | how long in-flight calls may finish after stdin closes |
| `ACOS_MCP_QUIET` | unset | silence stderr logging |
| `ACOS_ART_MEM_MAX` | `2G` | per-session guest cgroup memory ceiling (also the budget the host admission check accounts for) |
| `ACOS_MEMORY_GUARD` | on | `off` disables the `HOST_MEMORY_LOW` admission check (the cgroup ceiling still applies) |
| `ACOS_SESSION_DAEMON` | unset | `1` forwards session tools to a **running external** `acos serve` at `ACOS_SESSION_SOCKET`. Without it the server spawns its own private `acos serve --engine art` (real ART) on first use |
| `ACOS_SESSION_SOCKET` | `<runtime dir>/session.sock` | socket of that external daemon |
| `ACOS_MCP_ENGINE` | unset (`art`) | `legacy` keeps the frozen in-process Rust VM / RN-bridge backend (diagnostics only; no real Android). Never chosen implicitly |

---

## 1b. Which engine do the tools run?

**Real ART, by default.** The MCP session tools (`acos_launch`, `acos_observe`,
`acos_observe_tree`, `acos_tap`, `acos_text`, `acos_key`, `acos_logs`,
`acos_status`, `acos_reset`, `acos_destroy`, and the network module's
`acos_net` — same name and arguments as the session API's verb) do not run on the
frozen in-process
Rust VM any more. On the first session tool call the server lazily spawns **its
own private** `acos serve --engine art` daemon on a socket under
`$XDG_RUNTIME_DIR/acos/mcp-<pid>-<rand>/session.sock` and forwards every session
call to it, so the reply carries `engine: "art"`, `fidelity: "art-reference"`
and the real launch ladder to `first_frame`, and `acos_reset` really restarts
the guest in a fresh app-data directory (`data_cleared: true`).

That daemon is a child of the server: `PR_SET_PDEATHSIG(SIGTERM)` is set when it
is spawned, and on MCP stdin EOF/exit the server destroys its sessions, sends
`SIGTERM` (the daemon closes every ART guest), reaps the child and removes the
socket directory. Nothing outlives `acos-mcp`. **The same teardown runs on
`SIGTERM`/`SIGINT`** (FB5): a client that is killed never closes stdin, and a
supervisor stops the server with `SIGTERM` — both destroy exactly the sessions
this server launched, and no foreign `acos serve` is signalled.

### The daemon must be the same build as the server (REL3D)

A daemon speaking an older build's protocol is a wrong-answer risk (MODA11Y: an
older `acos serve` answered a newer client `code 5 unsupported` for a service the
new build implements), so the server never forwards to one:

* every daemon reports its identity in `hello`/`ping` — crate version plus a
  build id (`git sha` + a digest of the workspace sources + a hash of the
  binary, computed once at start) — and `DaemonClient` sends its own and
  compares **before any request**;
* the private daemon is checked the same way right after it is spawned. It is
  the `acos` binary (`ACOS_BIN`, the sibling of `acos-mcp`, or `PATH`), which
  can be a different build than the server; the server then refuses it and the
  tool call fails with `DAEMON_VERSION_MISMATCH` naming both identities instead
  of answering from the other build;
* with `ACOS_SESSION_DAEMON=1` an **external** daemon of another build is not
  attached: one stderr line carries the structured warning
  `{"code":"DAEMON_VERSION_MISMATCH","client":{…},"daemon":{…},"action":"private ART daemon (this server's own)"}`
  and the server uses its own private ART daemon instead.

The warning/error carries `client` and `daemon` identities (`version`, `git_sha`,
`build_id`, `exe`/`exe_hash`, `pid`, `auto_started`), so a report can name both
builds. `acos doctor` prints the CLI's identity next to a running daemon's.

### Session hygiene (FB5)

Sessions cost memory (up to `ACOS_ART_MEM_MAX` each), so the server reclaims the
ones it owns:

* **Idle TTL.** A session untouched for `ACOS_SESSION_IDLE_TTL_S` (default 600 s;
  `0` disables) is destroyed by the server — one stderr line, the guest is gone.
  `acos_launch {"idle_ttl_s": N}` overrides it per session; every later MCP call
  on the session renews the clock. The launch reply reports `idle_ttl_s` (and
  `label`), so the deadline is visible.
* **An expired session says so.** The next call on it returns
  `SESSION_NOT_FOUND` with `details.reason: "idle_ttl_expired"`, the
  `idle_ttl_s` it missed, and a hint to relaunch — never a generic "unknown id".
  `acos_launch` on that id starts a fresh session.
* **`acos_status` lists this server's sessions** under `sessions[]`:
  `session_id`, `label`, `package`, `activity`, `age_s`, `idle_s`,
  `idle_ttl_s`, `guest_pid`, `guest_rss_kb` (the guest's own `VmRSS`) and
  `live`. Use `acos_launch {"label": "..."}` to give a parked session a name you
  can recognise.
* **Per-rung `ms`.** The launch ladder reports `ms` on every rung (and
  `total_ms`/`ms_basis`), so "how long did each stage take" is answerable
  without a separate timing call.

The in-process backend still exists but is **frozen and opt-in only**:
`ACOS_MCP_ENGINE=legacy` selects it; it is for diagnostics and cannot run a real
APK. `ACOS_SESSION_DAEMON=1` is unchanged: it connects to an **external**
`acos serve` (any `--engine`) at `ACOS_SESSION_SOCKET` when one is listening. If
that daemon is not listening the server falls back to its own private ART
daemon.

Default session ids are unique per server process (`mcp-<pid>`), so two agents
sharing one external daemon never address each other's session.

### The `engine` argument

`acos_verify` advertises an `engine` argument, an enum of `art` (default, real
ART) and `rust-vm` (the frozen Rust VM). Omitted means `art`; the reply's
`fidelity` says which engine actually ran (`art-reference` or `native-runtime`),
and `required_fidelity: "art-reference"` on a non-ART run is refused
(`UNSUPPORTED_BACKEND`), never passed.

The **scenario YAML `engine:` field uses the same two words** (`art` /
`rust-vm`) and is metadata about what the scenario was written for; the
tool/CLI argument selects the execution engine. The legacy spellings
(`native-preferred`, `native-only` → `rust-vm`; `reference-only` → `art`) still
parse and produce a scenario warning naming the current word.

### Provenance: every reply names its inputs

`acos_verify` and `acos_launch` results carry a `provenance` block (also written
into `verdict.json` and printed in the CLI's summary line):

```json
"provenance": {
  "scenario": {"path": "/abs/s.yaml", "sha256": "<64 hex>", "sha256_12": "<12 hex>"},
  "apk":      {"path": "/abs/app.apk", "sha256": "<64 hex>", "sha256_12": "<12 hex>"},
  "engine": "art", "fidelity": "art-reference", "warnings": []
}
```

Paths are absolute and `launch` sets `scenario: null` (it has no scenario). A
warning is structured, never a fail: `SCENARIO_APP_OVERRIDDEN` means the
scenario's own `app:` names a different file than the `--apk` that ran. Use
`sha256_12` to check that the build you just made is the binary the verdict is
about.

### Unknown arguments are rejected

Every tool's `inputSchema` is a closed object (`additionalProperties: false`)
and the server enforces the same rule: an argument the tool does not have is
`INVALID_ARGUMENT`, naming the argument, the valid ones and a near-miss
suggestion. A silently ignored argument (for example `acos_observe_tree
{"screenshot": true}` on a build without it) reads as a success for work that
never happened, so it is refused instead.

---

## 1c. `changes`: what an action did

`acos_tap`, `acos_text`, `acos_key` and `acos_swipe` return `result.changes`
**by default** (`diff: false` omits it): the settled UI-tree diff of the action,
so the ordinary step is one call instead of tap-then-observe.

```json
"changes": {
  "baseline": "session",
  "frames": {"before": 7, "after": 9},
  "activity": "dev.acos.fixtures.counter.MainActivity",
  "focused": {"id": "n6", "text": "", "content_desc": "",
              "class": "android.widget.Button", "bounds": [42, 368, 273, 494]},
  "added": [{"id": "n9", "text": "Settings", "content_desc": "Settings tab",
             "class": "android.widget.TextView", "bounds": [0, 2200, 360, 2337]}],
  "removed": [{"id": "n5", "text": "Home", "content_desc": "Home tab",
               "class": "android.widget.TextView", "bounds": [0, 2200, 360, 2337]}],
  "changed": [{"id": "n3", "text": "1", "content_desc": "",
               "class": "android.widget.TextView", "bounds": [42, 317, 63, 368],
               "fields": {"text": {"old": "0", "new": "1"}}}],
  "counts": {"added": 1, "removed": 1, "changed": 1},
  "truncated": false
}
```

Rules (they are part of the contract, not implementation detail):

* **No extra cost.** The diff is computed from two trees the session already
  holds — the tree bound before the input was injected and the tree the
  action's own settle produced. It never triggers a traversal, an extra capture
  or an extra guest round trip, and it does not change the settle the action was
  going to take anyway.
* **No invented diffs.** When the session held no pre-action tree (an action
  right after a launch that bound none, or after a reset) the reply carries
  `changes: {"baseline": "none"}` with empty lists and zero counts. It never
  reports the whole tree as "added".
* **Identity is the stable id path, not list position.** A node's identity is a
  chain of segments joined with `/` from the root: the Android `resource_id`
  when the node has one, else `class#N` where `N` is its ordinal among
  same-class *unnamed* siblings. A duplicate identity inside one tree is
  disambiguated `~N` in document order. A named node therefore keeps its
  identity when a different sibling is added or removed, while a node that
  really moved (or was replaced) is reported as removed + added — never as an
  edit of its neighbour.
* **Compared fields** for a matched node: `text`, `content_desc`, `hint`,
  `class`, `resource_id`, `bounds`, `visible_bounds`, `visible`, `enabled`,
  `focused`, `clickable`, `scrollable`, `checked`, `selected`. Each changed one
  is reported as `{"old": …, "new": …}`; a `changed` entry always has at
  least one field.
* **Bounded.** Each of `added`/`removed`/`changed` stops at **40 entries**.
  `counts` always carries the true totals and `truncated: true` says a list was
  cut, so a whole-screen navigation still fits the MCP reply bound (REL3b) and
  a capped list can never be read as "that was everything".
* **`diff: false`** returns the reply with **no `changes` key**, byte for byte
  otherwise. A non-boolean `diff` is `INVALID_ARGUMENT`. The same reply is what a
  CLI `acos tap`/`acos key`/`acos text` prints through the session daemon
  (`acos serve`), since both share one engine.

## 1d. `handled` and `target`: did the app consume it, and what was there

`state_changed: false` used to be ambiguous: it could mean *"nothing tappable
there / no handler consumed the press"* or *"the app handled it and nothing
changed"*. Every `acos_tap`/`acos_key`/`acos_text`/`acos_swipe` reply now
separates the two:

```json
"handled": false,
"handled_evidence": {"source": "guest", "published": 2, "acked": 2,
                     "unhandled": [11, 12]},
"target": {
  "selector": {"text": "Floating button"},
  "node":   {"id": "n5_…:id/title", "class": "android.widget.TextView",
             "text": "Floating button", "content_desc": "", "bounds": "[42,342][669,405]",
             "clickable": false, "scrollable": false, "enabled": true,
             "focused": false, "window": "main"},
  "point":  {"x": 355, "y": 373},
  "hit": {"method": "tree-geometry",
          "node":    {"class": "android.widget.TextView", "clickable": false, …},
          "clickable": false,
          "consumer": null,
          "suggest": {"id": "…:id/toggle", "class": "android.widget.Switch",
                      "text": "Floating button", "bounds": "[669,338][1038,409]",
                      "clickable": true, …}}
},
"handled_hint": "nothing tappable there: the node at this point is android.widget.TextView «Floating button» at [42,342][669,405], which is not clickable and neither is any ancestor, and the app's window answered `finishInputEvent(handled) = false` …; tap the nearest clickable android.widget.Switch … instead"
```

* **`handled` is the framework's answer, never a heuristic.** It is the bit the
  app window passes to Android's `InputEventReceiver.finishInputEvent(handled)`
  (AOSP `ViewRootImpl` sets it from `FLAG_FINISHED_HANDLED`, and a dropped event
  finishes with `false`). It arrives on the real `InputChannel` `FINISHED`
  message, one ack per published `InputMessage`.
  * any unhandled ack → `false` (nothing consumed that event);
  * every published event acked handled → `true`;
  * a short or missing ack list → `null` **unknown**, never "not handled".
  **Except** when the host recorded that the window this input was delivered to
  was removed during the step (a dialog dismissed by `back`): its
  `InputEventReceiver` is disposed before it can finish the event, so the removal
  is the consumer's proof and the answer is `true` with
  `handled_evidence.source: "host_window_removed"` — that outranks even an
  all-unhandled ack set, and such a step never carries `INPUT_UNHANDLED`.
  `handled_evidence` always travels with it: `source` (`guest` for an ack,
  `host_window_removed` for the host's removal fact, `none` when nothing was
  delivered), `published`, `acked` and the `unhandled` sequence numbers.
* **`target` says what was there.** `node` is the node the selector resolved to
  (for a coordinate tap there is none); `hit` is the tree-geometry hit-test at
  the injected point from the same tree — the deepest visible node whose rect
  contains it (`node`), whether it is itself `clickable`, the nearest clickable
  ancestor-or-self (`consumer`), and, when nothing on that branch is clickable,
  the nearest clickable **sibling** (`suggest`; a switch beside a row's label
  wins over a nearer button in the next row). `hit.clickable: false` +
  `hit.consumer: null` is the "nothing tappable there" case.
* **`handled_hint`** explains the combination in one line, and only ever quotes
  the evidence above: nothing tappable + the node to tap instead; a clickable
  control that still refused the press (disabled or intercepted); a consumed
  input with no visible change (asynchronous handler or unchanged state); or an
  unknown answer.
* **`state_changed` is about the screen, `handled` is about the app.** Read
  `handled` first: with `handled: false` a `state_changed: false` is the correct
  outcome, not a broken tap.
* **Scenario steps** publish the same facts: every `tap`/`type`/`key`/`back`
  step JSON has an `input` block with `handled`, `handled_evidence`, `target`
  and `expect_change` — which is `"auto"` for a tap (its own state-change rule
  always runs) or the scenario's explicit `true`/`false`.

## 1d-bis. `acos_swipe`: a real gesture, and `scroll_until`

On the ART engine `acos_swipe` publishes a real interpolated `MotionEvent`
stream on the live input channel: `DOWN`, one `MOVE` every display refresh
interval (~16 ms) with real `eventTime` values, then `UP` — the same
`IWindowSession` channel `acos_tap` uses. The app's `VelocityTracker` and
`OverScroller` see a genuine finger, so `ListView`/`RecyclerView`/RN `FlatList`
fling with their own physics instead of jumping.

```json
{"from": {"x": 300, "y": 1100}, "to": {"x": 300, "y": 400},
 "duration_ms": 300, "fling": true}
{"direction": "up", "distance": 900}
{"direction": "up", "until": {"text": "Row 80"}, "max_attempts": 12}
```

* `from`/`to` are display pixels; `direction` (`up`/`down`/`left`/`right`) with
  an optional `distance` covers the common scroll.
* `duration_ms` defaults to 400 (120 with `fling: true`). A fling is a short,
  fast gesture — the coast is the list's, not the tool's.
* `handled` is REL3E's per-event `finishInputEvent` bit:
  `handled_evidence.published == acked` for every published event and
  `unhandled` lists the sequences the window dropped. A 300 ms swipe is ~20
  events.
* `until: <selector>` is a bounded `scroll_until`: swipe, take a fresh **settled**
  traversal, look for the node, repeat (`max_attempts`, 1..50). The reply carries
  `until: {selector, found: true, attempts, frame_seq}`. A miss is
  `ELEMENT_NOT_FOUND` (every attempt used, or the swipe was not consumed:
  `details.gesture_consumed: false` with the hint "gesture not consumed (swipe
  missed the scroller?)"), or `END_OF_LIST` when **two consumed** swipes in a row
  moved nothing — the list ended, `details.end_of_list: true`,
  `details.unchanged_gestures: 2` — with `details.attempts`,
  `details.max_attempts`, `details.last_frame_seq` and
  `details.last_tree_texts`. `until` refuses `settle: fast`/`content` because
  neither can prove a settled observation.
* `fling`/`direction`/`distance`/`until`/`max_attempts` on the frozen Rust VM are
  refused (`until` explicitly: only ART has settled-observation proof).


## 1e. Watching a session (live view)

Every live ART session publishes one small file
(`$XDG_RUNTIME_DIR/acos/live/<session_id>-<daemon_pid>.json`: client, project
dir, APK, package, activity, display, and the session's `stream_socket`) and
streams its screen and an action/event feed to any local viewer that connects to
that socket. The viewer is **view-only** and optional: with nobody watching the
session costs what it cost before (ADR-0027). The wire protocol — subscribe
line, framing, `H`/`F`/`E` messages, cost rules, registry fields, stale
handling — is `docs/LIVE_VIEW.md`; `tools/view/stream_probe.py` is a stdlib
reference consumer. The MCP tools themselves are unchanged and do not expose the
stream.

To watch it, start the browser hub in a terminal:

```
$ acos view
acos view: http://127.0.0.1:54321/?t=<token>
```

It binds 127.0.0.1 only, requires that random token on every request (no token
or a wrong one is `403`), and serves one self-contained page: the grid of live
sessions (client/agent, project dir, APK, activity, age) and, for the session
you click, its live screen with tap ripples, the current activity and the recent
action feed. Open the printed URL in any browser, or paste it into **T3 Code's
preview pane** (ADR-0027: no patch, no plugin). The page is view-only — no input
is ever forwarded to a guest — and the hub subscribes to a session's
`stream_socket` only while a tab has it open, one upstream subscription shared by
every tab, so an unwatched session still costs nothing. `acos view --once
--json` prints the session list and exits. Details: `docs/LIVE_VIEW.md` §8.

---

## 2. Tools

The protocol defines 13 commands (`acos_<command>` in
`acos_protocol::mcp_tools()`); the server adds two meta tools
(`acos_capabilities`, `acos_help`) and the server-local tools (the in-process
session tools plus `acos_doctor`). The number *advertised* in `tools/list`
depends on what the installed `acos` binary implements — see above; the table
below is the full structural list.

Whether a protocol command can run **today** is derived at runtime by probing
`acos --help`, never hard-coded, so new commands appear as available as soon as
the CLI implements them. Each `tools/list` entry carries the live `available`
and `fidelity` annotations: run `acos_capabilities` (or `tools/list`) for the
current set. The structural list below is generated from the descriptors, and
the `docs_tool_table_matches_descriptors` test fails if the docs drift from the
server.

<!-- ACOS_TOOLS_BEGIN -->

| tool | kind | command | fidelity | required args |
|---|---|---|---|---|
| `acos_inspect` | protocol | `inspect` | `static-analysis` | `apk`, `base` |
| `acos_run_dex` | protocol | `run-dex` | `native-runtime` | `base`, `path` |
| `acos_run` | protocol | `run` | `native-runtime` | `apk`, `base` |
| `acos_observe` | protocol | `observe` | `native-runtime` | `base` |
| `acos_capture` | protocol | `capture` | `native-runtime` | `base` |
| `acos_action` | protocol | `action` | `native-runtime` | `action`, `base` |
| `acos_assert` | protocol | `assert` | `native-runtime` | `assertion`, `base` |
| `acos_events` | protocol | `events` | `native-runtime` | `base` |
| `acos_reset` | protocol | `reset` | `native-runtime` | `base`, `scope` |
| `acos_profile` | protocol | `profile` | `native-runtime` | `base` |
| `acos_destroy` | protocol | `destroy` | `native-runtime` | `base` |
| `acos_verify` | protocol | `verify` | `native-runtime` | `artifacts`, `base`, `scenario` |
| `acos_capability_report` | protocol | `capability-report` | `static-analysis` | `apk`, `base` |
| `acos_capabilities` | server | - | `n/a` | - |
| `acos_help` | server | - | `n/a` | - |
| `acos_net` | server | - | `native-runtime` | - |
| `acos_launch` | server | - | `native-runtime` | - |
| `acos_observe_tree` | server | - | `native-runtime` | - |
| `acos_tap` | server | - | `native-runtime` | - |
| `acos_key` | server | - | `native-runtime` | - |
| `acos_text` | server | - | `native-runtime` | - |
| `acos_swipe` | server | - | `native-runtime` | - |
| `acos_logs` | server | - | `native-runtime` | - |
| `acos_status` | server | - | `native-runtime` | - |
| `acos_doctor` | server | - | `n/a` | - |
| `acos_services` | server | - | `native-runtime` | `op` |
| `acos_service_ctl` | server | - | `native-runtime` | `module`, `op` |

<!-- ACOS_TOOLS_END -->

Regenerate the block with `cargo run -p acos-mcp --example gen-tools-docs`.

`acos_verify` also accepts `engine: "art"` (mapped to the CLI's `--engine`).
`art` is the default in the tool, in the schema and in `acos verify` itself
(one constant: `acos_protocol::commands::DEFAULT_VERIFY_ENGINE`), so the CLI and
the tool never disagree about which engine a run used. Pass `engine: "rust-vm"`
only for the frozen VM.

`acos_verify` accepts `animations: "on"|"off"` and `native: true` (REL3A), the
same options `acos verify --animations/--native` have. Before that the tool
schema lacked both while the engine's own hints told agents to use
`animations: off`, so a run that hit a settle timeout had to leave MCP for a
shell. `animations: off` zeroes Android's three framework animation scales for
functional checks; it cannot stop an app that animates in its own code (RN
`Animated`, a spinner) — a `settle: content` step is the rule for those.

**Settle semantics (REL3A).** Every step JSON states the rule it used and how it
ended: `settle: {mode, settled, waited_ms, reason}`. `wait_for` and the tree/text
assertions (`assert_text`, `assert_visible`, `assert_not_visible`,
`assert_state`) default to *auto* — strict, then the content-stable rule when
strict never settles — and honour `timeout_ms`. A step whose settle timed out is
never a pass: in a scenario it is `status: timeout` with error class
`infra_timeout`; the live channel answers `acos_tap`/`acos_text`/`acos_key` with
`status: timeout`, `details.reason: "settle_timeout"` and the delivered facts in
`details.delivery`. Content stability backs tree/text/state claims only: a pixel
claim (`assert_region`, `assert_screenshot`) on a content-settled frame fails
with `details.reason: "content_settle_pixel"`, and a bounds claim with
`"content_settle_geometry"`. See `docs/AGENT_GUIDE.md` ("Settle modes").

A run owns its `artifacts` directory: a non-empty directory written by another
run is refused with `INVALID_ARGUMENT` naming the previous run id
(`error.details.previous_run_id`). Pass `overwrite: true` to discard that
evidence deliberately (`--overwrite` on the CLI). See `docs/AGENT_API.md` §10.4.

### `resources` and `prompts`

The same guide is served as MCP resources, for clients that read resources
instead of calling tools:

| uri | content |
|---|---|
| `acos://guide` | the complete agent guide (markdown) |
| `acos://guide/workflow`, `/mcp`, `/cli`, `/scenarios`, `/errors`, `/limits` | one section |
| `acos://errors` | the error-code playbook (alias of `/errors`) |

`resources/list` advertises all of them with `mimeType: text/markdown`,
`resources/templates/list` advertises `acos://guide/{topic}`, and
`resources/read` returns `contents[0].text`. An unknown URI is JSON-RPC
`-32602` (never an empty read).

One prompt, `verify_android_flow`, takes `apk` and `goal` and returns the
instruction for the whole verify loop (write the scenario, run `acos_verify`,
branch on `error.code`, report what the evidence proves). `prompts/get` without
its arguments is `-32602`.

`initialize` advertises `capabilities.resources` and `capabilities.prompts`
alongside `capabilities.tools`; both are supported on `2025-03-26` and
`2025-06-18`.

The server-local tools are served in-process (they never shell out):
`acos_launch` boots an APK into a session that persists across calls and returns
the stage ladder plus the **compact screen** by default (counts and visible texts;
`{"full": true}` returns the previous inline tree, and `tree_path` always names
the full tree file); `acos_observe_tree`/`acos_tap`/`acos_key`/`acos_text`/`acos_swipe`/`acos_logs`/`acos_status`
drive it; and `acos_doctor` runs the `acos doctor --json` host preflight (Android
SDK/build-tools, `android.jar`, JDK, workspace helper, debug keystore,
boot-classpath cache, host memory) and returns each failing check's `fix`.

`acos_logs` returns an **agent-relevant default view**: records at priority `W`
and above plus anything that names the app's own package, with `filter` naming
the defaults that were applied. The guest buffer is untouched —
`logs/logcat.txt` in a verify run is still the full log — and `"log_path"` in a
live session reply names that complete file. `{"all": true}` (or
`min_priority: "V"`) returns every record, subject to the 20 KiB reply cap.

### Service modules (ADR-0029)

`acos_launch {"services": [{"id": "feedback.vibrator", "profile": "stub"}]}`
attaches modules before the guest boots (`core` is always attached first at
`real`). The launch reply carries `modules[]` — `id`, `profile`, `attach`
(`hot`/`at-launch`), `state` and `fidelity` — and that is what was **actually
attached**, not what was requested. `acos_services {"op": "list",
"base": {"session_id": …}}` returns the same rows for a live session;
`{"op": "attach"|"detach", "id": …}` changes the set and answers with the new
one. Only `hot` modules can change live: an `at-launch` module (e.g. `core`) is
refused with a structured error naming the launch call, never silently ignored.
`acos_capabilities {"session_id": …}` includes `session.attached` and
`session.modules` for that session (the catalog alone when no session is named).

A module at `stub` or `scripted` is a documented stand-in, not a real
implementation. Its fidelity is reported in the launch reply, `acos_services`
and every `verdict.json` that used it (plus a `notes` line saying the pass
relied on it). A scenario step that declares `requires_services` is
`unsupported` when the module is `off`, never a pass. The generated module
catalog and the gap table are in `docs/AGENT_GUIDE.md` (`topic: "limits"`) and
`acos_capabilities.modules`.

The three **meta** tools (`acos_help`, `acos_capabilities`, `acos_doctor`)
report `fidelity: "n/a"`: they say nothing about the app under test, so
`static-analysis` would imply the reply came from the APK. `n/a` never satisfies
a fidelity requirement and is never a pass.

### Reply size: the compact screen, paging, and tree files

Every MCP tool and the CLI's `--json` output obey one rule (REL3C). A
tree-bearing reply is **not** a dump of the hierarchy:

* the default `tree` is the **compact screen**: every visible node with a
  non-empty `text` or `content_desc`, in document (reading) order, carrying its
  tree `id`, Android `resource_id`, `class`, `bounds`, `text`/`content_desc`,
  `clickable`, `window` and `package`. Structural wrappers (a `DecorView`, a
  `FrameLayout` with no words) carry no answer and are omitted;
* the compact list is **paged**, never head/tail-cut. The first page carries
  `next_cursor` (also `tree_next_cursor`); pass `{"cursor": N}` to read the next
  page. A middle-of-screen row is on page 3, not lost in the middle;
* the **full tree** is always written to the session's
  `tree/<frame_seq>.json`, and the reply names the absolute `tree_path`
  (live sessions get a per-session artifacts directory; verify runs keep
  `tree/<frame_seq>.json` in `--artifacts`). Read that file when you need the
  hierarchy, ids of non-text nodes, or bounds of an invisible node;
* `acos_launch` defaults to the compact summary/visible texts; `{"full": true}`
  (or `{"full_tree": true}`) returns the old inline tree when it fits. The full
  tree is still the file above. `acos_observe_tree`, `acos_observe` and
  `acos_reset` take the same arguments;
* the hard reply cap is **20 KiB** (`acos_protocol::reply::MAX_REPLY_BYTES`). If
  even a requested full tree or a long log list does not fit, the reply keeps
  the compact/paged view and the path, and `_meta.acos_mcp.reply_capped` says
  how many bytes were removed. A reply never silently exceeds the cap.

`acos_logs` uses the same principle: the default view is small (`W` + the app's
package), while `log_path` names the complete `logs/logcat.txt` in the session
artifacts. `{"all": true}` returns the whole in-memory view (subject to the
20 KiB cap and `logs_truncated`), not a different file.

### Getting a screenshot through the observe verbs

`acos_observe_tree` and `acos_observe` both take `screenshot`:

```json
{"name": "acos_observe_tree", "arguments": {"session_id": "mcp-1234", "screenshot": true}}
{"name": "acos_observe_tree", "arguments": {"session_id": "mcp-1234", "screenshot": "/abs/out/home.png"}}
```

* `true` writes `<session artifacts>/screenshots/<verb>-<frame_seq>.png`; a
  **relative** path resolves against the session's artifacts directory (never the
  daemon's cwd) and an **absolute** path must stay inside it. `..` components, a
  symlink that leaves the directory and a destination that is the frame artifact
  itself are refused with `INVALID_ARGUMENT` before anything is created or
  written — a screenshot can never create directories or overwrite files outside
  the session. Anything that is not `true` or such a path (`7`, `""`) is
  `INVALID_ARGUMENT` — never a silent no-op.
* The screenshot is always bound to the frame of the tree in the same reply: a
  request that asks for a screenshot makes the tree traversal capture pixels
  itself, so the reply's `screenshot.frame_seq` equals the tree's. A bound tree
  that has no pixels of its own is `NO_FRESH_FRAME` (refused) rather than paired
  with a newer capture.
* The reply's `result.screenshot` carries `path` (absolute), `bytes`, `md5`,
  `frame_seq`, `width`/`height` from the PNG's IHDR, and the coordinate mapping
  `display_size` + `display_origin`: PNG pixels are display pixels shifted by
  `display_origin` (`png_x = display_x - origin.x`), the same offset the input
  channel applies, so a tap by eye is not off by the status bar.
* The bytes are the launcher's own lossless artifact (60-140 KB for a 1080x2400
  frame), copied — never re-encoded.
* A placeholder frame (window exists, app content never mounted) has no app
  pixels: the call answers `NO_FRESH_FRAME` with `details.reason =
  "placeholder-content"` instead of writing the splash.
* The MCP reply also inlines the bounded preview (see *Content items* below) and
  a text line naming the lossless file, so the agent gets pixels and a path in
  one call.

### A launch that never mounts is `blocked`, not `pass`

`acos_launch`,`acos_reset` (and `acos_verify`'s `launch`/`reset` step) report
`status: "blocked"` with `error.code = PLACEHOLDER_CONTENT` when the window and
its decor exist but the app's own content never mounts. The label needs evidence
beyond the tree: the frame is the bare window skeleton (<= 16 nodes, no app text)
**and** nothing about the frame moved while the whole first-frame budget ran —
no newer `frame_number`/`frame_seq` and the same pixel digest. A text-free
skeleton whose pixels *did* change (a custom `View`, a `SurfaceView`/GL surface,
an image-only activity, Flutter without semantics) is a normal pass with a
`warning` (`code: PLACEHOLDER_SUSPECT`, `result.first_frame.placeholder_suspect:
true`), never a blocked verdict. The reply keeps the ladder in `result` for
diagnosis: `stage_reached: "window"`, `content: {"mounted": false,
"reason": "placeholder-content"}`, the `first_frame` rung rewritten to
`blocked`, `error.details` with `nodes`, `traversals`, `waited_ms`, `budget_ms`,
`frames_advanced`, `pixels_changed`, and `error.details.log_hint` with the one
guest log line that explains it (`UnsatisfiedLinkError`, `SoLoader`, `couldn't
find`, `FATAL`, else the first `E` line).

Never retry a placeholder launch with a longer timeout: the launch already spent
its whole `ACOS_ART_FIRST_FRAME_MS` budget (default 5000 ms) and the frame never
changed, so a bigger budget only reproduces the same answer more slowly. The
budget is for the opposite case — a **slow React Native bundle**: a launch whose
first paint lands after a second is waited for, and the wait is what makes the
first frame the app's content instead of the skeleton. Read the log line, then
`acos_inspect`.

A placeholder label is never sticky: once a later frame carries content, the
session answers from the current frame (a second `acos_launch`, a `reset` or a
screenshot are judged on what is on screen now).

### `acos_inspect` cross-checks the native libraries

`acos_inspect` stays static, but it now also answers "does the APK ship the
native libraries it asks for?": `result.native_lib_check` =

```json
{"abi": "x86_64", "platform_libs_ignored": 27,
 "warnings": [{"code": "NATIVE_LIB_MISSING", "lib": "libworklets.so",
               "abi": "x86_64", "source": "elf-needed",
               "referenced_by": ["libgesturehandler.so", "libreanimated.so"]}]}
```

References come from two places inside the APK: the ELF `DT_NEEDED` entries of
the libraries the APK ships for that ABI (`source: "elf-needed"`), and the string
arguments of the app's own `System.loadLibrary`/`SoLoader.loadLibrary` call sites
in the DEX (`source: "dex-load-library"`). A reference that resolves to a
`lib/<abi>/` entry is never a warning, and platform libraries (`libc.so`,
`libm.so`, …: the documented `SYSTEM_LIBS` set) are never warnings, so a healthy
APK answers `warnings: []` and a non-empty list is a packaging bug — usually the
cause of a `PLACEHOLDER_CONTENT` launch.

Every `tools/list` entry carries the protocol `description`, an `inputSchema`
generated from the request type, and annotations:

```json
{
  "name": "acos_inspect",
  "description": "... [MCP: available=true fidelity=static-analysis command=inspect]",
  "inputSchema": { "...": "..." },
  "annotations": {
    "title": "acos inspect",
    "readOnlyHint": true,
    "destructiveHint": false,
    "idempotentHint": true,
    "openWorldHint": false,
    "available": true,
    "fidelity": "static-analysis"
  },
  "_meta": {
    "acos": {
      "available": true,
      "fidelity": "static-analysis",
      "command": "inspect",
      "tool": "acos_inspect",
      "reason": null
    }
  }
}
```

`acos_capabilities` re-probes the backend (`{"refresh": true}`) and reports the
supported/unsupported set, the resolved binary and version. `acos_help` returns
the **agent guide** — [`docs/AGENT_GUIDE.md`](AGENT_GUIDE.md), embedded into the
binary at build time (`include_str!`), so the installed server carries it with no
files to find. It covers the loop (doctor → inspect → launch →
observe/observe_tree → tap/text/key/swipe → verify → reset → destroy), every CLI
command with a runnable example, how to write a scenario YAML (step types,
selectors, settle modes, `expect_change`, assertion kinds), every error code and
its next action, and the limits (one support row per route for native Views,
React Native and Flutter; rotate unsupported, x86_64 native libs, API 34, memory
admission). `{"topic": "workflow"|"mcp"|"cli"|"scenarios"|"errors"|"limits"}`
returns just that section.

`{"topic": "mcp"}` is special: it is **generated from the tool descriptors on
every reply**, not a section of the markdown guide. It lists every tool
`tools/list` advertises for this build with its required and optional arguments,
one paste-able example call and what the reply contains, plus the recipes that
used to be unanswerable from the tool list (how to get a screenshot today, how
to type into a field, how to find a control by `content_desc`, how to prove a
flow). A tool this build does not implement is absent from the table, so the
guide never teaches a call that answers `unsupported`.

---

## 3. Calling conventions

Each tool's `arguments` are exactly the ACOS request object. Identity fields are
under `base`; only `base.request_id` is required.

```json
{
  "base": { "request_id": "req_1", "budget": { "wall_ms": 30000 } },
  "apk": "fixtures/build/apps/hello-views.apk"
}
```

The tool result contains the ACOS **envelope** as one text item. When the
negotiated revision is `2025-06-18`, the same envelope is also returned as
`structuredContent`:

```json
{
  "content": [{ "type": "text", "text": "{\"ok\":true,\"status\":\"pass\",...}" }],
  "structuredContent": { "ok": true, "status": "pass", "fidelity": "static-analysis", "...": "..." },
  "isError": false
}
```

`isError` is `true` for every non-`pass` status (`fail`, `error`, `unsupported`,
`crash`, `timeout`, `blocked`). The envelope always has `ok`, `status`,
`backend`, `fidelity`, `capabilities_used` and `metrics`; a non-pass status
always carries an `error` with a stable `code`, an actionable `hint`,
`retryable` and optional `details`. **Branch on `error.code`, not on `message`.**

Evidence paths under the artifacts directory are returned as MCP
`resource_link` items (2025-06-18) or text lines. A PNG image (a screenshot, a
capture) is additionally inlined as an MCP `image` content item — but only as a
**bounded preview**: at most `ACOS_MCP_PREVIEW_MAX_SIDE` px on the long edge
(default 1280) and at most `ACOS_MCP_MAX_IMAGE_BYTES` bytes (default 512 KiB).
A full 1080x2400 capture is downscaled to 576x1280 before it is inlined; the
reply also carries a text line naming both sizes and the path of the **lossless**
file (which is still linked), so an agent reads the exact pixels from disk when
it needs them instead of paying for a multi-megabyte base64 blob in its context.
A file that does not decode as a PNG is only linked, never inlined. JPEG/WebP
evidence is linked, not inlined (no JPEG/WebP encoder is available offline).

### Artifacts and retention

`acos_verify` writes one run directory per scenario under `ACOS_ARTIFACTS_DIR`
(§10 of `docs/AGENT_API.md`). Retention applies to MCP runs automatically:

* a **passing** run keeps the verdict, `engine.json`, `retention.json`, the step
  documents, the UI trees, the logs, the events and the frames its assertions or
  captures declare (plus the first and last frame). Retained frames are
  re-encoded losslessly, so a run that used to cost 0.5–2 GB costs single-digit
  MiB;
* a **failing, timing-out, erroring or unsupported** run keeps every artifact —
  that is the diagnosis an agent reads next;
* no retained JSON names a pruned file. `retention.json` says what was kept or
  pruned (`decision`, `kept_bytes`, `pruned_bytes`,
  `kept_because_referenced`), and the verdict's `notes` repeat the summary.

The server never prunes the collection itself. To bound old runs (a long agent
session accumulates them), run the CLI against the same root:

```bash
acos artifacts gc --dir "$ACOS_ARTIFACTS_DIR" --older-than 24h --keep-last 2 --dry-run
```

It never touches a non-pass run younger than `--older-than` nor the newest
`--keep-last` runs per scenario, and prints the freed bytes. Drop `--dry-run` to
prune.

### Example session

```jsonc
--> {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"agent","version":"1"}}}
<-- {"jsonrpc":"2.0","id":1,"result":{
      "protocolVersion":"2025-03-26",
      "capabilities":{"tools":{"listChanged":false}},
      "serverInfo":{"name":"acos-mcp","version":"0.1.0"},
      "instructions":"ACOS exposes an agent-oriented Android verification API..."}}

--> {"jsonrpc":"2.0","method":"notifications/initialized"}

--> {"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
<-- {"jsonrpc":"2.0","id":2,"result":{"tools":[ /* 23 tool descriptors */ ]}}

--> {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
      "name":"acos_inspect",
      "arguments":{"base":{"request_id":"r1"},"apk":"fixtures/build/apps/hello-views.apk"}}}
<-- {"jsonrpc":"2.0","id":3,"result":{
      "content":[{"type":"text","text":"{\"ok\":true,\"status\":\"pass\",\"fidelity\":\"static-analysis\",\"result\":{\"package\":\"dev.acos.fixtures.helloviews\",...}}"}],
      "isError":false}}

--> {"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
      "name":"acos_run_dex",
      "arguments":{"base":{"request_id":"r2"},"path":"fixtures/build/dexprogs/01_hello/classes.dex","main":"Main"}}}
<-- {"jsonrpc":"2.0","id":4,"result":{
      "content":[{"type":"text","text":"{\"ok\":true,\"status\":\"pass\",\"fidelity\":\"native-runtime\",\"result\":{\"exit_code\":0,\"stdout\":\"Hello, ACOS!\\n...\"}}"}],
      "isError":false}}

--> {"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"acos_verify","arguments":{...}}}
<-- {"jsonrpc":"2.0","id":5,"result":{
      "content":[{"type":"text","text":"{\"ok\":false,\"status\":\"unsupported\",\"error\":{\"code\":\"UNSUPPORTED_BACKEND\",\"hint\":\"This ACOS build does not implement `verify`...\"}}"}],
      "isError":true}}
```

A full captured transcript is in
[`results/mcp/transcript.txt`](../results/mcp/transcript.txt); the availability
snapshot is in [`results/mcp/tools.md`](../results/mcp/tools.md) and
[`results/mcp/capabilities.json`](../results/mcp/capabilities.json).

---

## 4. Protocol details

* **Revisions.** The server defaults to `2025-03-26` and also understands
  `2025-06-18`. If the client requests a known revision it is echoed; otherwise
  the server answers with `2025-03-26`, which the client may accept or close.
  `structuredContent`, resource links and the top-level `title` field are used
  only on `2025-06-18`.
* **Methods.** `initialize`, `ping`, `tools/list`, `tools/call`,
  `notifications/initialized`, `notifications/cancelled`, `logging/setLevel`.
  Unknown methods return JSON-RPC `-32601`; malformed JSON returns `-32700`;
  invalid params return `-32602`.
* **Batches.** JSON-RPC batch arrays are not supported; they are rejected with
  `-32600`. (Batching was removed from MCP in `2025-06-18`.)
* **Cancellation.** Send `notifications/cancelled` with the in-flight
  `requestId`. The worker kills the child process **and its whole process group**
  and replies with an `INTERNAL` envelope carrying `details.cancelled: true`.
* **Timeouts and limits.** A request budget (`base.budget.wall_ms`) is a hard
  wall-clock limit → `TIMEOUT`. The captured-output cap, queue wait and memory
  budget map to `RESOURCE_LIMIT`. Exceeding `base.budget.memory_mib` when the
  backend reports peak RSS converts the result to `RESOURCE_LIMIT`.
* **Concurrency.** At most `ACOS_MCP_MAX_CONCURRENCY` calls run at once;
  additional calls queue up to `ACOS_MCP_QUEUE_TIMEOUT_MS`, then return a
  structured `RESOURCE_LIMIT`.
* **Sessions.** A registry keyed by `session_id` with an idle TTL (FB5, see
  §1b) backs the session tools (`run`/`observe`/`action`/...). `inspect` and
  `run-dex` are one-shot.
* **Shutdown.** Closing stdin makes the server wait
  `ACOS_MCP_SHUTDOWN_GRACE_MS` for in-flight calls to finish, then cancels any
  remainder and exits.

---

## 5. Safety

* **No shell.** Children are spawned with an explicit argv.
* **Path allow-list.** Every path argument (`apk`, `path`, `scenario`,
  `artifacts`, ...) is resolved, `..` components are rejected, symlinks are
  resolved (a link that resolves outside the roots is rejected too), and the
  result must live under `ACOS_ALLOWED_ROOTS` (default: cwd + artifacts dir;
  `register-mcp.sh` sets it to `$HOME`). Violations return `UNSAFE_PATH`; see
  §1 for how to fix it.
* **Argument limits.** String arguments are capped
  (`ACOS_MCP_MAX_REQUEST_BYTES` bounds the whole frame), arrays/objects are
  bounded, nesting is bounded.
* **Resource limits.** Wall-clock, output size, concurrency and memory budgets
  are enforced; timed-out or cancelled children are killed as a process group.
* **Host memory admission.** Before starting an ART guest, ACOS compares
  `MemAvailable` (`/proc/meminfo`) with the session's memory budget
  (`ACOS_ART_MEM_MAX`, default 2 GiB): if admitting it would leave the host
  under 10% of `MemTotal`, the launch fails fast with `HOST_MEMORY_LOW`
  (retryable, `error.details` names the measured `MemAvailable`, `MemTotal` and
  the requested budget). There is **no queue and no session-count limit** —
  free memory and call again. Each session still runs in its own cgroup with the
  `ACOS_ART_MEM_MAX` ceiling. `ACOS_MEMORY_GUARD=off` disables only the
  admission check.
* **stdout discipline.** Only protocol frames are written to stdout; all logs
  and diagnostics go to stderr.

---

## 6. Tests and manual use

```bash
# protocol + dispatcher + cancellation + concurrency (in-process over pipes)
cargo test -p acos-mcp

# lint
cargo clippy -p acos-mcp --all-targets -- -D warnings

# manual smoke test (bash + the server, no jq/python)
cargo build -p acos-mcp
cargo build --release -p acos-cli
crates/acos-mcp/tests/smoke.sh

# printable transcript
crates/acos-mcp/tests/transcript.sh

# server overhead vs the bare CLI (p50/p95 over N inspect calls)
crates/acos-mcp/tests/bench-overhead.sh 50
```

Test fixtures `tests/fixtures/sleep.sh` and `tests/fixtures/lockcheck.sh` are
tiny `acos` doubles used to prove cancellation kills a long-running child and
that the concurrency gate serialises calls; they are not part of the product.

---

## 7. Current limitations

* `acos-mcp` exposes what the `acos` binary implements today. Everything else is
  honestly labelled `unsupported` — never a pass.
* The capability probe parses the clap `--help` listing; if the CLI changes its
  help layout, call the `acos_capabilities` tool with `refresh: true` after a
  rebuild. A future CLI could expose a machine-readable `--commands` list.
* MCP `resources` and `prompts` are advertised, but only for the agent guide
  (`acos://guide…`, `verify_android_flow`); artifacts are still returned as
  tool-result content, not as resources.
* No HTTP/SSE transport (stdio only).
