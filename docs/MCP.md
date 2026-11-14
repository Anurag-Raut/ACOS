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
  the path and the roots; nothing is read and no guest starts).

`register-mcp.sh` defaults the value to **`$HOME`** so an agent working in
another repository (for example `~/Projects/gaten`) can pass that repo's APK and
its own `scenarios/*.yaml` while still being unable to reach outside the home
directory. The artifacts directory (`ACOS_ARTIFACTS_DIR`) is always allowed in
addition, and a bare relative path is resolved against the server's working
directory first. On a shared or multi-user host, list the specific repositories
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
| `ACOS_MCP_MAX_TEXT` | `4000` | maximum bytes for one string in a tool reply. A longer string (an error message carrying a 200-frame java tail and a 400-line native tail) is head/tail-capped with a marker naming its original size and the artifact path, and an identical repeat is replaced by a back-reference, so the full text appears once. `0` disables the bound (the raw CLI behaviour). The guide text (`acos_help`) is exempt |
| `ACOS_MCP_SESSION_TTL_MS` | `600000` | idle session TTL |
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
`acos_status`, `acos_reset`, `acos_destroy`) do not run on the frozen in-process
Rust VM any more. On the first session tool call the server lazily spawns **its
own private** `acos serve --engine art` daemon on a socket under
`$XDG_RUNTIME_DIR/acos/mcp-<pid>-<rand>/session.sock` and forwards every session
call to it, so the reply carries `engine: "art"`, `fidelity: "art-reference"`
and the real launch ladder to `first_frame`, and `acos_reset` really restarts
the guest in a fresh app-data directory (`data_cleared: true`).

That daemon is a child of the server: `PR_SET_PDEATHSIG(SIGTERM)` is set when it
is spawned, and on MCP stdin EOF/exit the server destroys its sessions, sends
`SIGTERM` (the daemon closes every ART guest), reaps the child and removes the
socket directory. Nothing outlives `acos-mcp`.

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

The **scenario YAML `engine:` field is a different thing**: it is the legacy
native-vs-reference preference (`native-preferred` | `native-only` |
`reference-only`) and ART verification does not read it. Do not confuse the two;
the tool/CLI argument selects the execution engine.

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
| `acos_capabilities` | server | - | `static-analysis` | - |
| `acos_help` | server | - | `static-analysis` | - |
| `acos_launch` | server | - | `native-runtime` | - |
| `acos_observe_tree` | server | - | `native-runtime` | - |
| `acos_tap` | server | - | `native-runtime` | - |
| `acos_key` | server | - | `native-runtime` | - |
| `acos_text` | server | - | `native-runtime` | - |
| `acos_swipe` | server | - | `native-runtime` | - |
| `acos_logs` | server | - | `native-runtime` | - |
| `acos_status` | server | - | `native-runtime` | - |
| `acos_doctor` | server | - | `static-analysis` | - |
<!-- ACOS_TOOLS_END -->

Regenerate the block with `cargo run -p acos-mcp --example gen-tools-docs`.

`acos_verify` also accepts `engine: "art"` (mapped to the CLI's `--engine`).
Without it `acos verify` uses its default engine, which cannot honour an
ART-only scenario (`animations: off`); pass `engine: "art"` for real ART.

### `resources` and `prompts`

The same guide is served as MCP resources, for clients that read resources
instead of calling tools:

| uri | content |
|---|---|
| `acos://guide` | the complete agent guide (markdown) |
| `acos://guide/workflow`, `/cli`, `/scenarios`, `/errors`, `/limits` | one section |
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
the stage ladder; `acos_observe_tree`/`acos_tap`/`acos_key`/`acos_text`/`acos_swipe`/`acos_logs`/`acos_status`
drive it; and `acos_doctor` runs the `acos doctor --json` host preflight (Android
SDK/build-tools, `android.jar`, JDK, workspace helper, debug keystore,
boot-classpath cache, host memory) and returns each failing check's `fix`.

### Getting a screenshot through the observe verbs

`acos_observe_tree` and `acos_observe` both take `screenshot`:

```json
{"name": "acos_observe_tree", "arguments": {"session_id": "mcp-1234", "screenshot": true}}
{"name": "acos_observe_tree", "arguments": {"session_id": "mcp-1234", "screenshot": "/abs/out/home.png"}}
```

* `true` writes `<session artifacts>/screenshots/<verb>-<frame_seq>.png`; a path
  writes there and is made absolute. Anything else (`7`, `""`) is
  `INVALID_ARGUMENT` — never a silent no-op.
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

`acos_launch` (and `acos_verify`'s `launch` step) report
`status: "blocked"` with `error.code = PLACEHOLDER_CONTENT` when the window and
its decor exist but the app's own content never mounts (<= 16 nodes, no app text
anywhere, after the whole first-frame budget). The reply keeps the ladder in
`result` for diagnosis: `stage_reached: "window"`, `content: {"mounted": false,
"reason": "placeholder-content"}`, the `first_frame` rung rewritten to
`blocked`, and `error.details.log_hint` with the one guest log line that explains
it (`UnsatisfiedLinkError`, `SoLoader`, `couldn't find`, `FATAL`, else the first
`E` line). Never retry a placeholder launch with a longer timeout: read the log
line, then `acos_inspect`.

### `acos_inspect` cross-checks the native libraries

`acos_inspect` stays static, but it now also answers "does the APK ship the
native libraries it asks for?": `result.native_lib_check` =

```json
{"abi": "x86_64", "platform_libs_ignored": 24,
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
its next action, and the limits (rotate unsupported, Flutter/RN deferred, x86_64
native libs, API 34, memory admission). `{"topic": "workflow"|"cli"|
"scenarios"|"errors"|"limits"}` returns just that section.

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
* **Sessions.** A registry keyed by `session_id` with an idle TTL exists for the
  future stateful commands (`run`/`observe`/`action`/...). `inspect` and
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
