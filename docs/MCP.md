# ACOS MCP server

`acos-mcp` is a [Model Context Protocol](https://modelcontextprotocol.io) (MCP)
stdio server that exposes the ACOS agent API to AI coding agents. It turns every
ACOS command into an MCP tool, so an agent can call `acos_inspect`,
`acos_verify`, `acos_run`, `acos_observe`, ... directly instead of shelling out
to the `acos` CLI.

The server is deliberately thin: it does not re-implement the runtime. It
adapts the typed contract in [`crates/acos-protocol`](../crates/acos-protocol)
and dispatches to the `acos` binary. **Every command appears in `tools/list`**,
whether or not it is implemented yet; each descriptor is annotated with
`available: true|false` and its expected `fidelity`.

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

### Claude Code

```bash
claude mcp add acos \
  --env ACOS_BIN="$PWD/target/release/acos" \
  --env ACOS_ALLOWED_ROOTS="$PWD" \
  -- "$PWD/target/release/acos-mcp"
```

Use `claude mcp list` to confirm it is connected and `claude mcp remove acos` to
remove it.

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
| `ACOS_ARTIFACTS_DIR` | directory that holds produced artifacts (default `<cwd>/.acos-mcp-artifacts`) |

Server limits (all optional):

| variable | default | meaning |
|---|---|---|
| `ACOS_MCP_MAX_REQUEST_BYTES` | `4194304` | maximum size of one request frame |
| `ACOS_MCP_MAX_CONCURRENCY` | `4` | maximum concurrent backend calls |
| `ACOS_MCP_QUEUE_TIMEOUT_MS` | `5000` | how long a tool call waits for a slot |
| `ACOS_MCP_CALL_TIMEOUT_MS` | `120000` | default call budget when the request declares none |
| `ACOS_MCP_MAX_OUTPUT_BYTES` | `16777216` | maximum captured bytes per child stream |
| `ACOS_MCP_MAX_IMAGE_BYTES` | `524288` | maximum image inlined as MCP image content |
| `ACOS_MCP_SESSION_TTL_MS` | `600000` | idle session TTL |
| `ACOS_MCP_SHUTDOWN_GRACE_MS` | `2000` | how long in-flight calls may finish after stdin closes |
| `ACOS_MCP_QUIET` | unset | silence stderr logging |

---

## 2. Tools

Tools are the descriptors from `acos_protocol::mcp_tools()` (`acos_<command>`),
plus two server tools. Today the `acos` binary implements `inspect` and
`run-dex`; everything else returns a structured `unsupported` envelope. That
availability is derived at runtime by probing `acos --help`, not hard-coded, so
new commands appear as available as soon as the CLI implements them.

| tool | command | available today | fidelity |
|---|---|---|---|
| `acos_inspect` | `inspect` | **yes** | `static-analysis` |
| `acos_run_dex` | `run-dex` | **yes** | `native-runtime` |
| `acos_run` | `run` | no | `native-runtime` |
| `acos_observe` | `observe` | no | `native-runtime` |
| `acos_capture` | `capture` | no | `native-runtime` |
| `acos_action` | `action` | no | `native-runtime` |
| `acos_assert` | `assert` | no | `native-runtime` |
| `acos_events` | `events` | no | `native-runtime` |
| `acos_reset` | `reset` | no | `native-runtime` |
| `acos_profile` | `profile` | no | `native-runtime` |
| `acos_destroy` | `destroy` | no | `native-runtime` |
| `acos_verify` | `verify` | no | `native-runtime` |
| `acos_capability_report` | `capability-report` | no | `static-analysis` |
| `acos_capabilities` | server tool | **yes** | `static-analysis` |
| `acos_help` | server tool | **yes** | `static-analysis` |

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
the agent workflow (inspect → verify → read verdict → fix → rerun), the
error-code playbook, and the supported/unsupported lists.

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
`resource_link` items (2025-06-18) or text lines; small PNG/JPEG/WebP images are
inlined as MCP `image` content.

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
<-- {"jsonrpc":"2.0","id":2,"result":{"tools":[ /* 15 tool descriptors */ ]}}

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
  resolved, and the result must live under `ACOS_ALLOWED_ROOTS` (default: cwd +
  artifacts dir). Violations return `UNSAFE_PATH`.
* **Argument limits.** String arguments are capped
  (`ACOS_MCP_MAX_REQUEST_BYTES` bounds the whole frame), arrays/objects are
  bounded, nesting is bounded.
* **Resource limits.** Wall-clock, output size, concurrency and memory budgets
  are enforced; timed-out or cancelled children are killed as a process group.
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
* The server does not yet advertise MCP `resources` or `prompts` capabilities;
  artifacts are returned as tool-result content.
* No HTTP/SSE transport (stdio only).
