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
| `ACOS_ART_BOOT_IMAGE` | `acos-art` (`boot_image.rs`, `launch.rs`, H13) | `lz4` (default) / `shared-fixed` / `shared-aslr`. Shared modes ro-bind an uncompressed image dir and pass `-Ximage:/data/img/boot.art -Xnorelocate`; `shared-aslr` builds one relocated image per serve cohort and reuses it for that cohort's sessions | **Shared mapping and image ASLR policy**: `shared-fixed` keeps the base at `0x70000000` in every session (trusted only). `shared-aslr` uses ART's own ±128 MiB, 4 KiB-aligned window (**16 bits**, same shape as the stock zygote's per-boot choice) and changes the base per serve start. A session can read the base of its own mapping and therefore of its siblings in the same serve — exactly what a stock Android app learns from the zygote image. A bad value fails the launch |
| `ACOS_SHARED_BOOT_IMAGE` | `acos-art` (`launch.rs`, B3 compatibility) | Explicit directory for the **fixed-base** image; equivalent to `ACOS_ART_BOOT_IMAGE=shared-fixed` with that directory | Same fixed-base ASLR warning as `shared-fixed`; refused when combined with `shared-aslr` |
| `ACOS_ZYGOTE_APP` | `acos-art` (`launch.rs`, Z3) | Research/trusted only, **off by default**: runs the guest as a pre-initialised ART zygote that forks the app child | Binds an arbitrary host build dir into the guest and copies a native shim into the session's `<data>/shim`; adds `-Xzygote -Xhidden-api-policy:disabled`. Only point it at a dir you built from `research/zygote-proto/z3/`. Never for untrusted APKs: the zygote sandbox failed security review for untrusted code (ZS, 2026-11-12) |
| `ACOS_ART_XMS`, `ACOS_ART_XMX`, `ACOS_ART_HEAP_TARGET_UTILIZATION` | `acos-art` (`launch.rs`, Z7) | Research/trusted only, **off by default**: passes the value as the real ART argv `-Xms`/`-Xmx`/`-XX:HeapTargetUtilization=` to `dalvikvm64` | Changes the guest's heap policy; a too-small ceiling can make an app fail (OOM) or GC more. Values are validated and a bad value fails the launch. Unset = argv unchanged |
| `ACOS_ART_EARLY_GC`, `ACOS_ART_GC_DELAY_MS` | `acos-art` (`launch.rs` + guest `LaunchMain`, H2) | **Product default ON**: right after the first live frame the guest requests one ART background CC collection (`VMRuntime.requestConcurrentGC()`); `ACOS_ART_EARLY_GC=0` turns it off, `ACOS_ART_GC_DELAY_MS` delays it (research knob) | Runs guest-side ART heap work earlier than the app would (a real background collection that returns free region space to the OS). It changes when the guest releases its own pages, not what it can touch; no sandbox line moves. Unset = on; `0` = inert |
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

## Shared boot image (H13): what one session learns about another

`shared-aslr` maps **one read-only file** into every session of a serve cohort.
The file is prepared once by the first launch under `<ACOS_ART_DATA>/boot-image/`
(directory `0700`, files `0444`) and is `--ro-bind`-mounted at `/data/img`; the
guest cannot write it, so a compromised session cannot poison a sibling's
image. The host user who owns the data root can replace it, but that user is
inside the trust boundary that already controls the runtime and sysroot.

The security trade is address-space information, not data integrity:

* **Base entropy.** `shared-aslr` picks `0x70000000 ± 128 MiB` on a 4 KiB grid:
  65 536 positions = **16 bits per serve cohort**. This is ART's own
  `ART_BASE_ADDRESS_MIN/MAX_DELTA` window and the same shape as stock Android's
  zygote, which randomises the boot image once per boot and shares that one
  mapping with every app.
* **Cross-session.** Every session of the cohort sees the same image base, so an
  attacker who can read `/proc/self/maps` in one session learns the base used by
  its siblings. Stock Android has the same property for all apps in a boot.
  A new serve start chooses a new base; there is no cross-serve reuse.
* **Cross-serve.** The prepared image is not a secret at rest: it contains no
  per-session data and its base is written in the image header. The knob does
  not add a new host read surface; it removes the per-process relocation cost,
  not the guest's ability to inspect its own address space.
* **`shared-fixed` is different.** It keeps `0x70000000` in every session and
  across every serve. That is a real loss of image ASLR and remains
  trusted-workloads-only (see `results/art-runtime/ZS-security-review.md` F10).
* **Disk / cgroup.** A prepared cohort costs ~173 MiB on disk and its pages are
  file-backed page cache, not anonymous memory. A new cohort sweeps the previous
  cohort's images; a killed serve can leave one behind until the next cohort or
  manual cleanup.

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
