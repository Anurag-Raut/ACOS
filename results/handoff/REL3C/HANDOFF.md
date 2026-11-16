# REL3C — agent-sized replies (root cause C)

Branch: `rel3c`. Reply rule lives in `acos_protocol::reply` and is shared by every
MCP tool and the CLI's `--json` output. Cap: **20 KiB** per reply
(`acos_protocol::reply::MAX_REPLY_BYTES`, below the existing F11 24 KiB test
ceiling).

## What changed

* `crates/acos-protocol/src/reply.rs` (new): compact visible-text nodes in
  reading order (`id`, Android `resource_id`, class, text/content_desc, bounds,
  clickable, window/package), `cursor` paging, full-tree artifact writer, and the
  hard whole-reply cap. No head/tail cutting: a middle page is reached by cursor.
* `acos-mcp`: `tool_result` applies the same rule to every tool (and a final cap),
  `acos_launch`/`acos_observe_tree` take `full` + `cursor`; live log replies
  default to `W` + the app package and name `log_path`.
* `acos-cli` daemon (`engine/serve.rs`): launch/reset/observe/tap keep full trees
  under the per-session `artifacts/tree/<frame_seq>.json`, inline the compact
  screen, and name `tree_path` / `tree_next_cursor`; logs write the full
  `logs/logcat.txt` and name `log_path`.
* `acos-verify`: `write_tree` never replaces a richer tree with a one-node
  skeleton; a failed step whose bound tree is a skeleton refreshes once for
  evidence only (assertion/settle semantics unchanged).
* Docs: `docs/AGENT_GUIDE.md`, `docs/MCP.md`.

REL3B's provenance/validation work is untouched.

## Real Gaten run (`--engine art`)

APK: `/home/Qui/Projects/acos/.cache/gaten/gaten-dev-current-x86_64-release.apk`.
Before binary: main checkout `target/release/acos` (pre-change, 2026-11-13).

| run | reply bytes | tree inline | full tree |
|---|---|---|---|
| `launch-before.json` | 491,204 | 1 root / 243 recursive nodes | none |
| `launch-after.json` | 4,900 | 4 visible text nodes, `compact:true` | 244 recursive nodes at `tree_path` |

Commands (both through `tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`):

```sh
# after
./.cache/rel3c-target/release/acos launch --engine art --json --timeout 180 <apk>
# before
ACOS_RUNTIME_DIR=$PWD/.cache/rel3c-before-runtime /home/Qui/Projects/acos/target/release/acos \
  launch --engine art --json --timeout 180 <apk>
```

`verify.json` is the capped CLI reply (`reply_capped:true`, 824 bytes) pointing
at `verify/verdict.json`; the run passed and its tree artifacts are full trees,
e.g. `verify/tree/13.json` = 473 recursive nodes (not `[DecorView]`,
`truncated:false`). `verify/tree/4.json` = 175 nodes.

## Tests

All commands ran offline under `tools/guarded.sh`:

* `cargo test --offline -p acos-mcp` — all pass; new
  `tests/compact_reply.rs` builds a 400-node stub tree and fails before this
  change: reply under cap, middle marker on page 200, full artifact 400 nodes.
* `cargo test --offline -p acos-protocol -p acos-verify` — all pass; new reply
  unit tests and the richer-tree-artifact test.
* `cargo test --offline -p acos-cli --test verify --test serve_daemon` — pass.
* `cargo test --offline -p acos-cli --test session --test serve_daemon --test verify --test verify_engine_art` — pass (77 s session test included).
* `cargo test --offline -p acos-mcp --test guide` — 11/11 pass (guide stays under
  the 3 KiB skeleton budget).

## Notes

* `ACOS_MCP_MAX_TEXT=0` remains the documented no-bound escape hatch; the hard
  cap is disabled with it.
* `acos_help` is exempt (its guide text is the requested product) as before.
* Verify runs whose pretty `--json` reply exceeds 20 KiB return a pointer object
  (`reply_capped`, `verdict_path`, `artifacts_dir`); the complete verdict and
  artifacts are on disk.
