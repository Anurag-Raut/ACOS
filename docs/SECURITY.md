# Security notes — confinement, trust boundaries and the `ACOS_*` knobs

This file is the short, auditable answer to "what can an environment variable
change about ACOS's protection?" It complements `DECISIONS.md` (ADR-0018) and
the sandbox tests (`crates/acos-sandbox/tests/confine.rs`,
`crates/acos-mcp/tests/protocol.rs`).

## The invariant that no knob can weaken

Landlock, seccomp, the namespaces, the rlimits and the `require_confinement`
compile-time default **cannot be disabled by the environment**. The MCP server
re-execs a confined child whenever `confinement_active()` is false
(`crates/acos-mcp/src/session_backend.rs`), and `acos-native-loader` refuses to
map a guest `.so` outside confinement. There is no `ACOS_NO_CONFINE`-style
escape hatch, by design.

Everything below either **widens a limit**, **redirects a root**, or **loads
guest code**; each one is opt-in, and each one is a trust decision by whoever
sets it.

## Safety-relevant knobs

| Knob | Scope | What it changes | Trust requirement / mitigation |
|---|---|---|---|
| `ACOS_HOST_RSS_MAX` | `acos-vm` (`limits.rs`) | Host RSS ceiling for the VM guard | Widening raises the ceiling only; the process is still bounded by `tools/guarded.sh` (`--mem`, default 4G) at the cgroup level |
| `ACOS_WALL_MAX_SECS` | `acos-vm` | Wall-clock ceiling for one run | Widening lets a slow program run longer; the wrapper's `timeout` still applies |
| `ACOS_NATIVE_MEM_MAX` | `acos-vm` (`natives/native_mem.rs`) | Total bytes of synthetic native regions (default 256 MiB) | Also the cap `NewDirectByteBuffer` validates a caller's `capacity` against (W172-A), so raising it widens both |
| `ACOS_HEAP_MAX` | `acos-vm` (`heap`) | Guest heap slot ceiling | Widening only; the RSS guard still applies |
| `ACOS_MAX_OPEN_FDS`, `ACOS_MAX_BYTES_WRITTEN` | `acos-vm` (`vfs.rs`) | VFS fd / write budgets | Widening only within the session root |
| `ACOS_ALLOWED_ROOTS` | `acos-mcp`, `acos-cli` | Host directories the MCP/CLI may *read* (e.g. to open an APK). `:`-separated | Setting it to `/` exposes the host read surface to a tool caller. Only set it to the directories the caller actually needs |
| `ACOS_CACHE_DIR` | `acos-runtime` (`boot_image_cache.rs`) | Boot-image cache root (writable) | Must be a **real directory you own, not group/world-writable** (`mode & 0o022 == 0`); otherwise startup fails with `ROOT_WRITABLE_BY_OTHERS` / `ROOT_NOT_A_DIRECTORY` / `ROOT_NOT_OWNED` (see below, W172-A) |
| `ACOS_SESSION_ROOT` | `acos-vm` VFS, `acos-mcp`, `acos-cli` | The VFS data root — the **only writable path** a confined child gets | Same ownership/mode rule; validated before anything is created under it, so another user cannot swap app data under a live session |
| `ACOS_BOOT_DIR` | `acos-vm`, `acos-mcp`, `acos-native-loader` | Extracted boot classpath (read) | Same ownership/mode rule; a writable-by-others classpath would let another user swap the DEX a confined child loads. An invalid root disables the boot classpath rather than using it |
| `ACOS_RUNTIME_DIR`, `ACOS_SESSION_SOCKET` | `acos-cli serve`, `acos-mcp` | Daemon state dir and its Unix socket | Created `0700`; the socket must not pre-exist as another user's path |
| `ACOS_FLUTTER_LIB_DIR`, `ACOS_FLUTTER_DRIVER_DEX`, `ACOS_RN_DRIVER_DEX` | `acos-mcp`, `acos-runtime`, `acos-jni` | Guest code loaded into the confined child (engine lib, driver DEX) | Only point these at artifacts you built; the child still runs confined, but the code is not otherwise authenticated |
| `ACOS_FLUTTER_SCRATCH`, `ACOS_RN_SCRATCH` | `acos-mcp` | Scratch dirs for engine/RN staging (fallback when `ACOS_SESSION_ROOT` is unset) | Inside the session root by default |
| `ACOS_NO_BOOT_CACHE`, `ACOS_BOOT_CACHE_NO_IMAGE`, `ACOS_BOOT_CACHE_DEX_TABLES` | `acos-runtime` (`session_pool.rs`), `acos-cli` | Turn the boot cache (or parts of it) **off** | These only reduce trust in the cache — they are the safe direction. The cache payload is validated on restore and a crafted entry can only cause a cache miss |
| `ACOS_REQUIRE_FIXTURES` | `acos-test-support` | Turns a missing test fixture from a SKIP into a failure | Strengthens CI honesty; never weakens a product path |
| `ACOS_RENDER_SKIP_GOLDEN` | `acos-render` tests | Skips the golden/parity comparison | Test-only; a skipped comparison is never reported as a pass |

## Root directories: `ACOS_CACHE_DIR`, `ACOS_SESSION_ROOT`, `ACOS_BOOT_DIR`

All three go through `acos_runtime::private_root::ensure_private_root`, which
fails closed with a structured code before ACOS writes or reads anything:

| Code | Meaning |
|---|---|
| `ROOT_NOT_A_DIRECTORY` | The path is a symlink or a non-directory (a symlinked cache root is never followed) |
| `ROOT_NOT_OWNED` | The directory is owned by another uid |
| `ROOT_WRITABLE_BY_OTHERS` | `mode & 0o022 != 0`: another local user can add, remove or replace entries |
| `ROOT_IO_ERROR` | The metadata could not be read, or tightening to `0700` failed |

A directory that is readable (but not writable) by others — a normal `0755`
checkout or `~/.cache` — is accepted and tightened to `0700`. The
agent-facing error for a rejected session root is `UNSAFE_PATH` with
`details.root_error` carrying the code above.

Rationale: these roots decide *what code the confined child loads* and *what
app data it sees*. The sandbox makes the child unable to write outside its root;
this check makes the root itself trustworthy.

## Known residual trust boundaries

* **In-process embedders.** `NewDirectByteBuffer` validates the caller's
  `(address, capacity)` against `/proc/self/maps` and the
  `ACOS_NATIVE_MEM_MAX` cap before copying (W172-A), but the caller still owns
  keeping that buffer alive for the duration of the call. A native that frees
  the buffer concurrently can still fault its own process; that is a
  self-inflicted DoS inside the confined child, not a host escape.
* **Guest `.so` execution** is enabled only inside confinement; the loader
  resolves `DT_NEEDED` from the APK/VFS search paths and rejects absolute host
  paths outside them.
* **`ACOS_ALLOWED_ROOTS=/`** is a real widening of the read surface. It is
  accepted because an agent-driving host may legitimately need to read project
  files, but it should be narrow in shared environments.
