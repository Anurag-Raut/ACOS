# ACOS error codes (one table)

Canonical taxonomy: `crates/acos-protocol/src/error.rs` (`ErrorCode`); the JSON
Schemas under `crates/acos-protocol/schema/` are generated from it. Codes are
additive: never renamed or reused for a different meaning. Every code means
"this did not work"; only a real pass returns `ok: true` / exit 0.

**Exit status** is the process status when the code is the top-level failure:

- `1` — `acos launch/observe/tap/verify/...` prints an `ok:false` envelope (or a
  text error) and fails; a non-pass verdict is never 0.
- `70` — `acos run-dex` / `acos-vm` error envelope (`UNSUPPORTED_*` or
  `VM_INTERNAL`). The same code reached through `launch`/`verify` is `1`.
- `2` — `acos-art` harness error (`acos-art: <CODE>: <detail>`), and CLI usage
  errors. MCP has no process exit: the JSON-RPC result carries `ok:false` and
  the protocol status (`unsupported`, `blocked`, `fail`, `timeout`, `error`).

**ABI rule (ADR-0018 W59-A / E1).** An APK that ships native code but no
`lib/<host-abi>/` is `UNSUPPORTED_ABI` on every route (CLI gate, daemon RN
backend, Flutter session). `WRONG_ABI` is only the loader-level condition for a
single ELF whose `e_machine` the in-process loader cannot map.

| Code | Meaning | Component | Exit |
|---|---|---|---|
| `UNSUPPORTED_ANDROID_API` | The native engine lacks a required `android.*` API or behaviour. | acos-vm | 70 / 1 |
| `UNSUPPORTED_JAVA_API` | The native engine lacks a required Java/library API. | acos-vm | 70 / 1 |
| `UNSUPPORTED_BYTECODE` | A Dalvik opcode or bytecode feature is not implemented. | acos-vm | 70 / 1 |
| `UNSUPPORTED_NATIVE_SYMBOL` | A required native (ELF/Bionic/JNI) symbol is not implemented. | acos-native-loader / acos-vm | 70 / 1 |
| `NEEDS_REAL_DEVICE` | The behaviour needs a real device, OEM service or hardware target. | engine / CLI | 1 |
| `REQUIRES_GMS` | The app requires licensed Google Mobile Services. | engine / CLI | 1 |
| `WRONG_ABI` | One ELF's `e_machine`/class is not what the loader can map (per-library; not an APK ABI inventory miss). | acos-native-loader | 70 / 1 |
| `UNSUPPORTED_ABI` | The APK ships native code, but none for the host ABI (names the available ABIs). | acos-apk / acos-cli / acos-runtime / acos-jni / acos-mcp | 1 |
| `NO_DISPLAY` | Capture/observe was requested on a headless session. | acos-runtime / MCP | 1 |
| `NO_TEXT` | Text/semantics were requested but the chosen route cannot provide them. | acos-runtime / MCP | 1 |
| `UNSUPPORTED_RELOCATION` | A native relocation kind is not implemented by the loader. | acos-native-loader | 70 / 1 |
| `RENDER_UNSUPPORTED_OP` | The renderer hit an operation it cannot draw faithfully. | acos-render | 1 |
| `UNSUPPORTED_BACKEND` | The requested backend does not exist in this build. | acos-cli / MCP | 1 |
| `UNSUPPORTED_RN` | The APK is not structurally a React Native APK (no RN/Hermes runtime or no JS bundle). | acos-runtime / acos-mcp | 1 |
| `UNSUPPORTED_RN_SETUP` | A React Native session failed before the bridge mounted. | acos-runtime / acos-mcp | 1 |
| `UNSUPPORTED_RN_DRIVER` | The React Native Java driver failed to mount or drive the bridge. | acos-runtime / acos-mcp | 1 |
| `UNSUPPORTED_RN_NO_RENDERER` | React Native bridge has no native view managers, so it cannot produce a pixel frame (`observe --frame`). | acos-runtime / acos-mcp | 1 |
| `UNSUPPORTED_RN_KEY` | Key input is not wired through the React Native bridge. | acos-runtime | 1 |
| `UNSUPPORTED_RN_TEXT` | Text input is not wired through the React Native bridge. | acos-runtime | 1 |
| `UNSUPPORTED_RN_NO_TARGET` | A `tap --text`/`--testID` selector matched no node in the RN tree. | acos-runtime | 1 |
| `NO_FRESH_FRAME` | No frame satisfying the request (freshness / sequencing) exists. | acos-runtime / acos-render | 1 |
| `UNVERIFIED_FIDELITY` | Evidence cannot be tied to the required fidelity level. | acos-verify | 1 |
| `APK_NOT_ZIP` | The input is not a readable zip/APK container. | acos-apk | 1 |
| `APK_NO_MANIFEST` | The container is a zip but has no `AndroidManifest.xml`. | acos-apk | 1 |
| `APK_IO` | I/O failure while reading the APK from disk. | acos-apk | 1 |
| `APK_ENTRY` | A zip entry could not be read or decompressed. | acos-apk | 1 |
| `APK_SIGNING_MALFORMED` | The APK Signing Block is present but malformed. | acos-apk | 1 |
| `AXML_MALFORMED` | The binary `AndroidManifest.xml` (AXML) is malformed. | acos-apk | 1 |
| `ARSC_MALFORMED` | The `resources.arsc` resource table is malformed. | acos-apk | 1 |
| `DEX_BAD_MAGIC` | DEX magic is not `dex\n`. | acos-dex | 70 / 1 |
| `DEX_UNSUPPORTED_VERSION` | DEX version is not supported. | acos-dex | 70 / 1 |
| `DEX_BAD_CHECKSUM` | DEX Adler-32 checksum does not match. | acos-dex | 70 / 1 |
| `DEX_BAD_SIGNATURE` | DEX SHA-1 signature does not match. | acos-dex | 70 / 1 |
| `DEX_TRUNCATED` | DEX is truncated relative to its header/map. | acos-dex | 70 / 1 |
| `DEX_BAD_ENDIAN` | DEX endian tag is invalid. | acos-dex | 70 / 1 |
| `DEX_INDEX_OUT_OF_RANGE` | A DEX index is out of range. | acos-dex | 70 / 1 |
| `DEX_BAD_INSTRUCTION` | A DEX instruction is malformed. | acos-dex | 70 / 1 |
| `DEX_UNUSED_OPCODE` | A DEX opcode is defined as unused. | acos-dex | 70 / 1 |
| `DEX_BAD_LEB128` | A DEX LEB128 value is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_MUTF8` | A DEX MUTF-8 string is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_ENCODED_VALUE` | A DEX encoded value is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_CLASS_DATA` | A DEX class data item is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_CODE_ITEM` | A DEX code item is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_DEBUG_INFO` | DEX debug info is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_ANNOTATION` | A DEX annotation is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_MAP` | The DEX map is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_HEADER` | The DEX header is malformed. | acos-dex | 70 / 1 |
| `DEX_VERIFY_FAILED` | DEX structural or bytecode verification failed. | acos-dex | 70 / 1 |
| `DEX_OVERFLOW` | Integer overflow while parsing a DEX structure. | acos-dex | 70 / 1 |
| `DEX_LOAD_ERROR` | A DEX file could not be loaded into the VM. | acos-vm | 70 / 1 |
| `ELF_MALFORMED` | An ELF native library is malformed. | acos-native-loader | 70 / 1 |
| `NATIVE_DEP_NOT_FOUND` | A native dependency is missing. | acos-native-loader | 70 / 1 |
| `NATIVE_INIT_FAILED` | A native library initializer failed. | acos-native-loader | 70 / 1 |
| `NATIVE_IO_ERROR` | Native I/O failed. | acos-native-loader | 70 / 1 |
| `NATIVE_SYMBOL_NOT_FOUND` | A required native symbol was not found. | acos-native-loader | 70 / 1 |
| `NATIVE_INVALID_ARGUMENT` | A native-loader argument is invalid. | acos-native-loader | 70 / 1 |
| `VM_INTERNAL` | A VM invariant or implementation bug (never user input). | acos-vm | 70 / 1 |
| `JAVA_EXCEPTION` | A live Java exception escaped the entry point. | acos-vm | 70 / 1 |
| `APP_CRASHED` | The app process crashed (uncaught exception / signal / abort). | acos-vm / acos-runtime | 1 |
| `APP_NOT_RESPONDING` | The app is not responding (ANR / main-thread stall). | acos-runtime | 1 |
| `ASSERTION_FAILED` | An assertion ran and did not hold. | acos-verify | 1 |
| `ELEMENT_NOT_FOUND` | A selector matched no node. | acos-runtime / acos-verify | 1 |
| `STALE_REF` | A UI node reference is no longer valid. | acos-runtime | 1 |
| `INSUFFICIENT_HOST_EVIDENCE` | A tree-only assertion ran on an untrusted APK run, where the tree is guest-asserted; fail closed and require host-observed evidence. | acos-cli / acos-verify | 1 |
| `UNSUPPORTED_ASSERTION` | The requested assertion kind has no implementation in this build (for example `assert_region.text_ocr` with no OCR engine). | acos-verify | 1 |
| `SESSION_NOT_FOUND` | The requested session does not exist (or was already destroyed). | acos-runtime / MCP | 1 |
| `SESSION_BUSY` | The session is busy with another request. | acos-runtime / MCP | 1 |
| `ISOLATION_UNAVAILABLE` | The requested isolation level is not implemented for this app. | acos-runtime / acos-sandbox | 1 |
| `CONFINEMENT_UNAVAILABLE` | The process could not enter its confinement sandbox; native code is refused (fail closed). | acos-sandbox / acos-cli / acos-native-loader | 1 |
| `LANDLOCK_UNAVAILABLE` | The Landlock filesystem layer could not be assembled or proven on a kernel that supports it (fail closed). Distinct from `SECCOMP_UNAVAILABLE`. | acos-art / acos-sandbox | 2 |
| `ISOLATION_UNSATISFIED` | The requested isolation level is advertised but its guarantees are unmet. | acos-runtime / acos-sandbox | 1 |
| `RAM_GUARD` | A memory admission/guard rule was triggered. | acos-runtime / acos-art | 1 / 2 |
| `RAM_ABORT` | Execution was aborted by the RAM guard. | acos-runtime / acos-art | 1 / 2 |
| `TIMEOUT` | The operation exceeded its wall/CPU budget. | acos-runtime / acos-art | 1 / 2 |
| `RESOURCE_LIMIT` | A declared resource budget was exhausted. | acos-runtime / acos-art | 1 / 2 |
| `UNTRUSTED_APK_NOT_CERTIFIED` | An untrusted APK run was requested while the product path is not certified for untrusted APKs; only the labelled hostile-fixture test override may run it. | acos-cli | 1 |
| `INVALID_SCENARIO` | The scenario is structurally or semantically invalid. | acos-verify | 1 |
| `INVALID_ARGUMENT` | A request argument is invalid. | acos-cli / MCP | 1 |
| `UNSAFE_PATH` | An artifact path was rejected (escape, absolute path, symlink, ...). | acos-cli / acos-protocol | 1 |
| `ZIP_SLIP` | A native-library entry name attempts to escape its target directory. | acos-apk | 1 |
| `ARTIFACTS_INVALID` | The artifacts directory is incomplete or contradicts its verdict. | acos-verify | 1 |
| `RENDER_BAD_HANDLE` | A render handle is invalid or already released. | acos-render | 1 |
| `RENDER_FONT_NOT_FOUND` | A required font could not be found. | acos-render | 1 |
| `RENDER_INVALID_ARGUMENT` | A renderer argument is invalid. | acos-render | 1 |
| `RENDER_SIZE` | A render surface has invalid dimensions. | acos-render | 1 |
| `RENDER_DECODE` | An image could not be decoded. | acos-render | 1 |
| `RENDER_IO` | Renderer I/O failed. | acos-render | 1 |
| `RENDER_STRICT` | Strict rendering mode escalated an otherwise-recordable fidelity loss. | acos-render | 1 |
| `INTERNAL` | ACOS itself failed (distinct from an app crash). | acos-cli / harness | 1 |

## Codes outside the protocol enum

`acos-art` (real-ART harness) and `acos-arm64` have their own error enums and
emit these strings in `acos-art: <CODE>: ...` form. They are stable, but not
part of `acos-protocol::ErrorCode` yet; a consumer that branches on them must
match the string. `acos-art` exits 2 for all of them.

| Code | Meaning | Component | Exit |
|---|---|---|---|
| `MEMORY_CGROUP_UNAVAILABLE` | The fail-closed memory/pids cgroup could not be created. | acos-art | 2 |
| `SECCOMP_UNAVAILABLE` | The guest seccomp filter could not be built, installed, or proven (`OK guard=active` absent). | acos-art | 2 |
| `LANDLOCK_UNAVAILABLE` | The guest Landlock ruleset could not be assembled, applied, or proven. | acos-art | 2 |
| `UNSUPPORTED_SYSCALL` | The guest guard observed a denied syscall during a run (structured run result). | acos-art | 1 |
| `ARM64_UNAVAILABLE` | The arm64 qemu transport (qemu, bionic sysroot, or guard) could not be assembled. | acos-arm64 | 1 |

Adding a new code: add the `ErrorCode` variant + `as_str` + hint +
`ALL_ERROR_CODES` (and `default_status` if it is not `error`), regenerate the
schemas (`UPDATE_SCHEMA=1 cargo test -p acos-protocol schemas_are_up_to_date`),
then add one row here.
