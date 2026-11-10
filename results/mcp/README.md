# ACOS MCP results

Evidence for the `acos-mcp` MCP stdio server (`crates/acos-mcp/`). Every file
here was produced by running the real binary on this machine; no numbers are
estimated.

| file | produced by | meaning |
|---|---|---|
| `cargo-test.txt` | `cargo test -p acos-mcp` | unit + protocol + e2e test output (50 tests) |
| `clippy.txt` | `cargo clippy -p acos-mcp --all-targets -- -D warnings` | zero-warning lint run |
| `transcript.txt` | `crates/acos-mcp/tests/transcript.sh` | real MCP session: initialize, tools/list (truncated), `acos_inspect`, `acos_run_dex`, `acos_verify`→unsupported |
| `capabilities.json` | `acos_capabilities` over a real session | machine-readable availability snapshot |
| `tools.md` | derived from `capabilities.json` | the 15 tools with `available` true/false and fidelity |
| `overhead.txt` | `crates/acos-mcp/tests/bench-overhead.sh 50` | per-call server overhead vs the bare CLI (p50/p95 over 50 `inspect` calls) |

## Headline results

**Tests:** 50 passed, 0 failed.

```text
acos_mcp (lib)         31 passed
e2e (real binary)       1 passed
protocol (pipes)       18 passed
doc-tests               0 passed
```

**Clippy:** `Finished` with `-D warnings`; no warnings.

**Tools:** 15 descriptors. Available today: `acos_inspect`, `acos_run_dex`,
`acos_capabilities`, `acos_help`. Unavailable (structured `unsupported`, never a
pass): the other 11 protocol commands.

**Per-call overhead over the bare CLI** (50 iterations, `inspect` on
`fixtures/build/apps/hello-views.apk`, wall-clock ms around one call):

| | p50 | p95 | mean |
|---|---|---|---|
| bare `acos inspect --json` | 1 | 2 | 1.1 |
| MCP `tools/call acos_inspect` round trip | 3 | 4 | 3.2 |
| **server overhead** | **2** | **2** | — |

The host was shared with other agents during the run, so these are
order-of-magnitude numbers, not a controlled benchmark. The overhead is the
JSON-RPC framing + worker-thread handoff + the server's own process spawn of
`acos`; the backend child dominates neither at this fixture size (both are a few
milliseconds).

## Reproduce

```bash
cargo test -p acos-mcp 2>&1 | tee results/mcp/cargo-test.txt
cargo clippy -p acos-mcp --all-targets -- -D warnings 2>&1 | tee results/mcp/clippy.txt
crates/acos-mcp/tests/transcript.sh > results/mcp/transcript.txt
crates/acos-mcp/tests/bench-overhead.sh 50 > results/mcp/overhead.txt
```

## Fidelity and honesty notes

* `acos_inspect` is `static-analysis`: it parses the APK and proves nothing about
  runtime behaviour.
* `acos_run_dex` is `native-runtime`: the ACOS Rust VM really executed the DEX,
  but the result is not ART-validated.
* An unimplemented command returns `status: "unsupported"`, `ok: false`,
  `error.code: "UNSUPPORTED_BACKEND"` and `isError: true`. There is no code path
  that turns an unsupported command into a pass.
