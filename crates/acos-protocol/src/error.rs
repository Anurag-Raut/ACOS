//! The ACOS error taxonomy.
//!
//! One stable machine code, one actionable hint, one retryability flag. The
//! hints are written for an AI coding agent: they say what to do next, not what
//! went wrong internally.
//!
//! Every code emitted by an existing ACOS crate (`acos-apk`, `acos-dex`,
//! `acos-vm`, `acos-native-loader`, `acos-render`) is represented here so that
//! crates never have to invent an out-of-band string. [`EXISTING_CRATE_CODES`]
//! enumerates that compatibility set and a test enforces it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A stable, machine-readable ACOS error code.
///
/// The serde representation is the exact `SCREAMING_SNAKE_CASE` string that
/// agents branch on. Codes are additive: a code is never renamed or re-used
/// for a different meaning.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    // ---- unsupported capabilities (never a pass) -------------------------
    /// The native engine lacks a required `android.*` API or behaviour.
    UnsupportedAndroidApi,
    /// The native engine lacks a required Java/library API.
    UnsupportedJavaApi,
    /// A Dalvik opcode or bytecode feature is not implemented.
    UnsupportedBytecode,
    /// A required native (ELF/Bionic/JNI) symbol is not implemented.
    UnsupportedNativeSymbol,
    /// The behaviour needs a real device, OEM service or hardware target.
    NeedsRealDevice,
    /// The app requires licensed Google Mobile Services.
    RequiresGms,
    /// The APK/native library ABI is not supported by this engine.
    WrongAbi,
    /// The APK ships native code, but none for the host ABI (names the available ABIs).
    UnsupportedAbi,
    /// Capture/observe was requested on a headless session.
    NoDisplay,
    /// Text/semantics were requested but the chosen route cannot provide them.
    NoText,
    /// A native relocation kind is not implemented by the loader.
    UnsupportedRelocation,
    /// The renderer hit an operation it cannot draw faithfully.
    RenderUnsupportedOp,
    /// The requested backend does not exist in this build.
    UnsupportedBackend,
    /// The APK is not structurally a React Native APK (no RN/Hermes runtime or
    /// no JS bundle).
    UnsupportedRn,
    /// A React Native session failed before the bridge mounted.
    UnsupportedRnSetup,
    /// The React Native Java driver failed to mount or drive the bridge.
    UnsupportedRnDriver,
    /// React Native bridge has no native view managers, so it cannot produce a
    /// pixel frame (`observe --frame`).
    UnsupportedRnNoRenderer,
    /// Key input is not wired through the React Native bridge.
    UnsupportedRnKey,
    /// Text input is not wired through the React Native bridge.
    UnsupportedRnText,
    /// A `tap --text`/`--testID` selector matched no node in the RN tree.
    UnsupportedRnNoTarget,

    // ---- evidence / freshness --------------------------------------------
    /// No frame satisfying the request (freshness / sequencing) exists.
    NoFreshFrame,
    /// Evidence cannot be tied to the required fidelity level.
    UnverifiedFidelity,

    // ---- APK / AXML / ARSC ------------------------------------------------
    /// The input is not a readable zip/APK container.
    ApkNotZip,
    /// The container is a zip but has no `AndroidManifest.xml`.
    ApkNoManifest,
    /// I/O failure while reading the APK from disk.
    ApkIo,
    /// A zip entry could not be read or decompressed.
    ApkEntry,
    /// The APK Signing Block is present but malformed.
    ApkSigningMalformed,
    /// The binary `AndroidManifest.xml` (AXML) is malformed.
    AxmlMalformed,
    /// The `resources.arsc` resource table is malformed.
    ArscMalformed,

    // ---- DEX --------------------------------------------------------------
    /// DEX magic is not `dex\n`.
    DexBadMagic,
    /// DEX version is not supported.
    DexUnsupportedVersion,
    /// DEX Adler-32 checksum does not match.
    DexBadChecksum,
    /// DEX SHA-1 signature does not match.
    DexBadSignature,
    /// DEX is truncated relative to its header/map.
    DexTruncated,
    /// DEX endian tag is invalid.
    DexBadEndian,
    /// A DEX index is out of range.
    DexIndexOutOfRange,
    /// A DEX instruction is malformed.
    DexBadInstruction,
    /// A DEX opcode is defined as unused.
    DexUnusedOpcode,
    /// A DEX LEB128 value is malformed.
    DexBadLeb128,
    /// A DEX MUTF-8 string is malformed.
    DexBadMutf8,
    /// A DEX encoded value is malformed.
    DexBadEncodedValue,
    /// A DEX class data item is malformed.
    DexBadClassData,
    /// A DEX code item is malformed.
    DexBadCodeItem,
    /// DEX debug info is malformed.
    DexBadDebugInfo,
    /// A DEX annotation is malformed.
    DexBadAnnotation,
    /// The DEX map is malformed.
    DexBadMap,
    /// The DEX header is malformed.
    DexBadHeader,
    /// DEX structural or bytecode verification failed.
    DexVerifyFailed,
    /// Integer overflow while parsing a DEX structure.
    DexOverflow,
    /// A DEX file could not be loaded into the VM.
    DexLoadError,

    // ---- native loader ----------------------------------------------------
    /// An ELF native library is malformed.
    ElfMalformed,
    /// A native dependency is missing.
    NativeDepNotFound,
    /// A native library initializer failed.
    NativeInitFailed,
    /// Native I/O failed.
    NativeIoError,
    /// A required native symbol was not found.
    NativeSymbolNotFound,
    /// A native-loader argument is invalid.
    NativeInvalidArgument,

    // ---- VM / app ---------------------------------------------------------
    /// A VM invariant or implementation bug (never user input).
    VmInternal,
    /// A live Java exception escaped the entry point.
    JavaException,
    /// The app process crashed (uncaught exception / signal / abort).
    AppCrashed,
    /// The app is not responding (ANR / main-thread stall).
    AppNotResponding,

    // ---- assertions / interaction ----------------------------------------
    /// An assertion ran and did not hold.
    AssertionFailed,
    /// A selector matched no node.
    ElementNotFound,
    /// A UI node reference is no longer valid.
    StaleRef,

    // ---- sessions / isolation / resources --------------------------------
    /// The requested session does not exist (or was already destroyed).
    SessionNotFound,
    /// The session is busy with another request.
    SessionBusy,
    /// The requested isolation level is not implemented for this app.
    IsolationUnavailable,
    /// The process could not enter its confinement sandbox; native code is refused (fail closed).
    ConfinementUnavailable,
    /// The Landlock filesystem layer could not be assembled or proven on a kernel
    /// that supports it (fail closed). Distinct from `SECCOMP_UNAVAILABLE`.
    LandlockUnavailable,
    /// The requested isolation level is advertised but its guarantees are unmet.
    IsolationUnsatisfied,
    /// A memory admission/guard rule was triggered.
    RamGuard,
    /// Execution was aborted by the RAM guard.
    RamAbort,
    /// The operation exceeded its wall/CPU budget.
    Timeout,
    /// A declared resource budget was exhausted.
    ResourceLimit,

    // ---- request/scenario/artifact validity ------------------------------
    /// The scenario is structurally or semantically invalid.
    InvalidScenario,
    /// A request argument is invalid.
    InvalidArgument,
    /// An artifact path was rejected (escape, absolute path, symlink, ...).
    UnsafePath,
    /// A native-library entry name attempts to escape its target directory.
    ZipSlip,
    /// The artifacts directory is incomplete or contradicts its verdict.
    ArtifactsInvalid,

    // ---- renderer ---------------------------------------------------------
    /// A render handle is invalid or already released.
    RenderBadHandle,
    /// A required font could not be found.
    RenderFontNotFound,
    /// A renderer argument is invalid.
    RenderInvalidArgument,
    /// A render surface has invalid dimensions.
    RenderSize,
    /// An image could not be decoded.
    RenderDecode,
    /// Renderer I/O failed.
    RenderIo,
    /// Strict rendering mode escalated an otherwise-recordable fidelity loss.
    RenderStrict,

    // ---- harness ----------------------------------------------------------
    /// ACOS itself failed (distinct from an app crash).
    Internal,
}

impl ErrorCode {
    /// The exact wire string for this code.
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::UnsupportedAndroidApi => "UNSUPPORTED_ANDROID_API",
            ErrorCode::UnsupportedJavaApi => "UNSUPPORTED_JAVA_API",
            ErrorCode::UnsupportedBytecode => "UNSUPPORTED_BYTECODE",
            ErrorCode::UnsupportedNativeSymbol => "UNSUPPORTED_NATIVE_SYMBOL",
            ErrorCode::NeedsRealDevice => "NEEDS_REAL_DEVICE",
            ErrorCode::RequiresGms => "REQUIRES_GMS",
            ErrorCode::WrongAbi => "WRONG_ABI",
            ErrorCode::UnsupportedAbi => "UNSUPPORTED_ABI",
            ErrorCode::NoDisplay => "NO_DISPLAY",
            ErrorCode::NoText => "NO_TEXT",
            ErrorCode::UnsupportedRelocation => "UNSUPPORTED_RELOCATION",
            ErrorCode::RenderUnsupportedOp => "RENDER_UNSUPPORTED_OP",
            ErrorCode::UnsupportedBackend => "UNSUPPORTED_BACKEND",
            ErrorCode::UnsupportedRn => "UNSUPPORTED_RN",
            ErrorCode::UnsupportedRnSetup => "UNSUPPORTED_RN_SETUP",
            ErrorCode::UnsupportedRnDriver => "UNSUPPORTED_RN_DRIVER",
            ErrorCode::UnsupportedRnNoRenderer => "UNSUPPORTED_RN_NO_RENDERER",
            ErrorCode::UnsupportedRnKey => "UNSUPPORTED_RN_KEY",
            ErrorCode::UnsupportedRnText => "UNSUPPORTED_RN_TEXT",
            ErrorCode::UnsupportedRnNoTarget => "UNSUPPORTED_RN_NO_TARGET",
            ErrorCode::NoFreshFrame => "NO_FRESH_FRAME",
            ErrorCode::UnverifiedFidelity => "UNVERIFIED_FIDELITY",
            ErrorCode::ApkNotZip => "APK_NOT_ZIP",
            ErrorCode::ApkNoManifest => "APK_NO_MANIFEST",
            ErrorCode::ApkIo => "APK_IO",
            ErrorCode::ApkEntry => "APK_ENTRY",
            ErrorCode::ApkSigningMalformed => "APK_SIGNING_MALFORMED",
            ErrorCode::AxmlMalformed => "AXML_MALFORMED",
            ErrorCode::ArscMalformed => "ARSC_MALFORMED",
            ErrorCode::DexBadMagic => "DEX_BAD_MAGIC",
            ErrorCode::DexUnsupportedVersion => "DEX_UNSUPPORTED_VERSION",
            ErrorCode::DexBadChecksum => "DEX_BAD_CHECKSUM",
            ErrorCode::DexBadSignature => "DEX_BAD_SIGNATURE",
            ErrorCode::DexTruncated => "DEX_TRUNCATED",
            ErrorCode::DexBadEndian => "DEX_BAD_ENDIAN",
            ErrorCode::DexIndexOutOfRange => "DEX_INDEX_OUT_OF_RANGE",
            ErrorCode::DexBadInstruction => "DEX_BAD_INSTRUCTION",
            ErrorCode::DexUnusedOpcode => "DEX_UNUSED_OPCODE",
            ErrorCode::DexBadLeb128 => "DEX_BAD_LEB128",
            ErrorCode::DexBadMutf8 => "DEX_BAD_MUTF8",
            ErrorCode::DexBadEncodedValue => "DEX_BAD_ENCODED_VALUE",
            ErrorCode::DexBadClassData => "DEX_BAD_CLASS_DATA",
            ErrorCode::DexBadCodeItem => "DEX_BAD_CODE_ITEM",
            ErrorCode::DexBadDebugInfo => "DEX_BAD_DEBUG_INFO",
            ErrorCode::DexBadAnnotation => "DEX_BAD_ANNOTATION",
            ErrorCode::DexBadMap => "DEX_BAD_MAP",
            ErrorCode::DexBadHeader => "DEX_BAD_HEADER",
            ErrorCode::DexVerifyFailed => "DEX_VERIFY_FAILED",
            ErrorCode::DexOverflow => "DEX_OVERFLOW",
            ErrorCode::DexLoadError => "DEX_LOAD_ERROR",
            ErrorCode::ElfMalformed => "ELF_MALFORMED",
            ErrorCode::NativeDepNotFound => "NATIVE_DEP_NOT_FOUND",
            ErrorCode::NativeInitFailed => "NATIVE_INIT_FAILED",
            ErrorCode::NativeIoError => "NATIVE_IO_ERROR",
            ErrorCode::NativeSymbolNotFound => "NATIVE_SYMBOL_NOT_FOUND",
            ErrorCode::NativeInvalidArgument => "NATIVE_INVALID_ARGUMENT",
            ErrorCode::VmInternal => "VM_INTERNAL",
            ErrorCode::JavaException => "JAVA_EXCEPTION",
            ErrorCode::AppCrashed => "APP_CRASHED",
            ErrorCode::AppNotResponding => "APP_NOT_RESPONDING",
            ErrorCode::AssertionFailed => "ASSERTION_FAILED",
            ErrorCode::ElementNotFound => "ELEMENT_NOT_FOUND",
            ErrorCode::StaleRef => "STALE_REF",
            ErrorCode::SessionNotFound => "SESSION_NOT_FOUND",
            ErrorCode::SessionBusy => "SESSION_BUSY",
            ErrorCode::IsolationUnavailable => "ISOLATION_UNAVAILABLE",
            ErrorCode::ConfinementUnavailable => "CONFINEMENT_UNAVAILABLE",
            ErrorCode::LandlockUnavailable => "LANDLOCK_UNAVAILABLE",
            ErrorCode::IsolationUnsatisfied => "ISOLATION_UNSATISFIED",
            ErrorCode::RamGuard => "RAM_GUARD",
            ErrorCode::RamAbort => "RAM_ABORT",
            ErrorCode::Timeout => "TIMEOUT",
            ErrorCode::ResourceLimit => "RESOURCE_LIMIT",
            ErrorCode::InvalidScenario => "INVALID_SCENARIO",
            ErrorCode::InvalidArgument => "INVALID_ARGUMENT",
            ErrorCode::UnsafePath => "UNSAFE_PATH",
            ErrorCode::ZipSlip => "ZIP_SLIP",
            ErrorCode::ArtifactsInvalid => "ARTIFACTS_INVALID",
            ErrorCode::RenderBadHandle => "RENDER_BAD_HANDLE",
            ErrorCode::RenderFontNotFound => "RENDER_FONT_NOT_FOUND",
            ErrorCode::RenderInvalidArgument => "RENDER_INVALID_ARGUMENT",
            ErrorCode::RenderSize => "RENDER_SIZE",
            ErrorCode::RenderDecode => "RENDER_DECODE",
            ErrorCode::RenderIo => "RENDER_IO",
            ErrorCode::RenderStrict => "RENDER_STRICT",
            ErrorCode::Internal => "INTERNAL",
        }
    }

    /// What an AI coding agent should do next on this code.
    pub const fn hint(self) -> &'static str {
        match self {
            ErrorCode::UnsupportedAndroidApi => {
                "Inspect the exact API in details.api/details.at; avoid it, guard it with a version/feature check, or mark the task unsupported. Never count it as a pass."
            }
            ErrorCode::UnsupportedJavaApi => {
                "Read details.api/details.at; replace it with an implemented API, or declare the task unsupported and (if allowed) escalate to the reference backend."
            }
            ErrorCode::UnsupportedBytecode => {
                "Read details.opcode/details.at; the VM cannot execute this bytecode yet. Reduce the code feature or report the exact opcode as a coverage gap."
            }
            ErrorCode::UnsupportedNativeSymbol => {
                "Read details.symbol/details.library; implement/import the symbol, rebuild the .so for the supported ABI, or report the exact missing symbol. Do not fake a return value."
            }
            ErrorCode::NeedsRealDevice => {
                "This behaviour requires a real/OEM device or hardware. Report it as unsupported; do not claim a native or simulated pass."
            }
            ErrorCode::RequiresGms => {
                "The app depends on licensed Google Mobile Services. Use a GMS-enabled reference target or report unsupported."
            }
            ErrorCode::WrongAbi => {
                "Compare the APK ABI with the engine ABI. Supply a build containing the supported ABI, or run an ABI that is actually supported."
            }
            ErrorCode::UnsupportedAbi => {
                "The APK has native libraries for other ABIs only. Use an ABI-split or universal build that includes the engine ABI (x86_64), or do not request native loading."
            }
            ErrorCode::NoDisplay => {
                "Create the session with a display (offscreen:WxH) before capture/observe, or drop the step for a headless logic-only scenario."
            }
            ErrorCode::NoText => {
                "The chosen route has no text. Enable the text/semantics capability or use structural selectors (id/class/bounds) instead."
            }
            ErrorCode::UnsupportedRelocation => {
                "Report the relocation type and library; use a build without it or extend the loader. Do not skip the load."
            }
            ErrorCode::RenderUnsupportedOp => {
                "Report the draw operation; simplify the view, or accept a lower fidelity only if the verdict says so explicitly."
            }
            ErrorCode::UnsupportedBackend => {
                "Choose a backend listed by capability-report; do not silently fall back. If escalation is allowed, record the backend actually used."
            }
            ErrorCode::UnsupportedRn => {
                "The APK is not structurally React Native (needs libreactnative*/libhermes plus assets/index.android.bundle). Do not force the RN path; inspect the APK layout."
            }
            ErrorCode::UnsupportedRnSetup => {
                "Read details.stage: the RN bridge could not stage the guest tree, DEX, JNI or JS bundle. Check the APK ABI, the driver DEX and the writable session root."
            }
            ErrorCode::UnsupportedRnDriver => {
                "Read details.stage: the RN bridge mounted but the Java driver failed. The RN Paper bridge is single-threaded; do not re-run main on a live session."
            }
            ErrorCode::UnsupportedRnNoRenderer => {
                "This RN bridge registers no native view managers, so UIImplementation never lays out or draws. Use observe --tree and tap --text/--testID; do not expect a pixel frame."
            }
            ErrorCode::UnsupportedRnKey => {
                "The RN bridge does not model key events yet. Use a tap/text selector instead."
            }
            ErrorCode::UnsupportedRnText => {
                "The RN bridge does not model text input yet. Use a tap selector instead."
            }
            ErrorCode::UnsupportedRnNoTarget => {
                "The selector matched no node in the current RN tree. Observe --tree first and use the exact text/testID it reports."
            }
            ErrorCode::NoFreshFrame => {
                "Do not reuse an older capture. Wait for a new frame after the action, or request capture with the correct after_seq/cause_id, then retry."
            }
            ErrorCode::UnverifiedFidelity => {
                "Evidence is not tied to the required fidelity (for example a JVM result for an ART requirement). Re-run on a qualifying backend or report unsupported."
            }
            ErrorCode::ApkNotZip => {
                "Verify the path is a readable APK/zip; rebuild the APK. This is not an app-behaviour failure."
            }
            ErrorCode::ApkNoManifest => {
                "The APK has no AndroidManifest.xml. Rebuild it with aapt2/gradle; do not attempt to run it."
            }
            ErrorCode::ApkIo => "Check the APK path and permissions; this is a host I/O failure, not an app failure.",
            ErrorCode::ApkEntry => "A zip entry is corrupt; rebuild the APK and retry inspect.",
            ErrorCode::ApkSigningMalformed => {
                "The signing block is malformed. Re-sign the APK; if this is a third-party APK, report it instead of installing it."
            }
            ErrorCode::AxmlMalformed => {
                "The binary manifest is malformed. Rebuild with aapt2; if third-party, record the parse gap."
            }
            ErrorCode::ArscMalformed => {
                "The resource table is malformed. Rebuild resources; use --no-resources only if the scenario does not need them."
            }
            ErrorCode::DexBadMagic => "Verify the file starts with `dex\\n`; pass a .dex/.apk, not a jar/class file.",
            ErrorCode::DexUnsupportedVersion => {
                "Recompile for a supported DEX version (035-039) or extend the parser. Do not treat it as executable."
            }
            ErrorCode::DexBadChecksum => {
                "Rebuild or re-sign the DEX; decide explicitly whether checksum enforcement is enabled (ADR-0017)."
            }
            ErrorCode::DexBadSignature => {
                "Rebuild the DEX; SHA-1 enforcement is on by default for Android semantics."
            }
            ErrorCode::DexTruncated => "The DEX is incomplete; rebuild it and retry.",
            ErrorCode::DexBadEndian => "Recompile the DEX for little-endian reverse-endian layout.",
            ErrorCode::DexIndexOutOfRange => "The DEX is corrupt; rebuild it. Report the offset if it came from a third party.",
            ErrorCode::DexBadInstruction => "The DEX instruction stream is malformed; rebuild and re-run verification.",
            ErrorCode::DexUnusedOpcode => "The DEX uses an unused opcode; rebuild with a standard d8 version.",
            ErrorCode::DexBadLeb128 => "The DEX encoding is corrupt; rebuild it.",
            ErrorCode::DexBadMutf8 => "A DEX string is corrupt; rebuild it.",
            ErrorCode::DexBadEncodedValue => "A DEX encoded value is corrupt; rebuild it.",
            ErrorCode::DexBadClassData => "The DEX class data is corrupt; rebuild it.",
            ErrorCode::DexBadCodeItem => "The DEX code item is corrupt; rebuild it.",
            ErrorCode::DexBadDebugInfo => "Debug info is corrupt; rebuild with or without debug info and retry.",
            ErrorCode::DexBadAnnotation => "An annotation is corrupt; rebuild the DEX.",
            ErrorCode::DexBadMap => "The DEX map is corrupt; rebuild the DEX.",
            ErrorCode::DexBadHeader => "The DEX header is corrupt; rebuild the DEX.",
            ErrorCode::DexVerifyFailed => {
                "Read the verifier reason; fix the source or bytecode, or record the exact verification gap. Do not execute unverified code as a pass."
            }
            ErrorCode::DexOverflow => "The DEX sizes overflow; rebuild it or report the exact structure.",
            ErrorCode::DexLoadError => {
                "Read details; ensure the DEX is on the classpath and the main class exists. This is a load failure, not an assertion failure."
            }
            ErrorCode::ElfMalformed => "The .so is malformed; rebuild it with a supported NDK/sysroot.",
            ErrorCode::NativeDepNotFound => "Add the missing dependency to the APK's lib/<abi>/ set or report it.",
            ErrorCode::NativeInitFailed => "Read the initializer failure; fix the JNI_OnLoad/constructor, or report unsupported.",
            ErrorCode::NativeIoError => "Check file permissions and paths for the native library.",
            ErrorCode::NativeSymbolNotFound => "Check the symbol name/version; rebuild the library or fix the JNI declaration.",
            ErrorCode::NativeInvalidArgument => "Fix the native-loader request arguments.",
            ErrorCode::VmInternal => {
                "This is an ACOS bug, not an app bug. Capture the request, scenario, events and stack; do not retry blindly; report it."
            }
            ErrorCode::JavaException => {
                "An exception escaped. Inspect exception/stack, fix the app (or the missing platform behaviour), and rerun."
            }
            ErrorCode::AppCrashed => {
                "Read crash.exception/stack and the last events. Fix the app or report the unsupported platform call; a crash is never a pass."
            }
            ErrorCode::AppNotResponding => {
                "The main thread is blocked. Inspect the ANR stack and recent events; fix the deadlock/long task, then rerun."
            }
            ErrorCode::AssertionFailed => {
                "Compare expected vs actual in the assertion diff, inspect the linked frame/tree, then fix the app or the expectation. Rerun the scenario."
            }
            ErrorCode::ElementNotFound => {
                "Re-observe the tree and check the selector (id/text/class/bounds). If the node is legitimately absent, fix the scenario expectation."
            }
            ErrorCode::StaleRef => {
                "Re-run observe/assert to obtain fresh node ids; never reuse node refs across a mutation."
            }
            ErrorCode::SessionNotFound => {
                "Create a session with run first; list/keep the session id. Destroying a session invalidates it."
            }
            ErrorCode::SessionBusy => {
                "Wait for the in-flight request to finish or use a separate session. Do not issue concurrent mutations on one session."
            }
            ErrorCode::IsolationUnavailable => {
                "Choose an isolation level this app/engine supports (shared-task < process < sandbox < core < vm), or report the missing guarantee."
            }
            ErrorCode::ConfinementUnavailable => {
                "Native code is refused because the process could not enter the seccomp/Landlock confinement. Do not retry unchanged; fix the confinement stack or launch without native loading."
            }
            ErrorCode::LandlockUnavailable => {
                "The kernel supports Landlock but the guest ruleset was not applied or its `OK landlock=active` evidence is missing. Do not treat the run as sandboxed; fix the Landlock plan/applier or use a kernel without Landlock (documented fallback)."
            }
            ErrorCode::IsolationUnsatisfied => {
                "The requested isolation is not actually guaranteed. Do not claim data independence; escalate or report unsupported."
            }
            ErrorCode::RamGuard => {
                "Free memory, lower --parallel, or raise the budget within policy; retry after the host has headroom."
            }
            ErrorCode::RamAbort => {
                "Execution was aborted by the RAM guard. Reduce concurrency/memory or run on a larger host; retry is safe."
            }
            ErrorCode::Timeout => {
                "Increase the step budget if the work is legitimate, or fix the hang. Retry after checking events; do not treat timeout as pass."
            }
            ErrorCode::ResourceLimit => {
                "A declared budget (memory/CPU/wall) was exhausted. Raise it explicitly or reduce the workload; report the measured value."
            }
            ErrorCode::InvalidScenario => {
                "Read every error path in the validation result and fix the YAML. Do not edit the runtime to accept an invalid scenario."
            }
            ErrorCode::InvalidArgument => {
                "Fix the named request argument. Read error.details for the field and accepted values."
            }
            ErrorCode::UnsafePath => {
                "Use a path inside the workspace/artifacts directory. Absolute paths, `..` and symlink escapes are rejected."
            }
            ErrorCode::ZipSlip => {
                "A native-library entry name escaped its target directory and was rejected. Treat the APK as hostile; do not extract it."
            }
            ErrorCode::ArtifactsInvalid => {
                "The artifacts directory contradicts its verdict. Re-run verify; never accept a pass without frame/tree/step evidence."
            }
            ErrorCode::RenderBadHandle => "Do not reuse released render handles; re-observe and retry.",
            ErrorCode::RenderFontNotFound => "Install/point at a font that supports the required script; rerun rendering.",
            ErrorCode::RenderInvalidArgument => "Fix the renderer argument; read details.",
            ErrorCode::RenderSize => "Use a valid display size (non-zero, within the budget) or fix the surface request.",
            ErrorCode::RenderDecode => "The image/font decode failed; replace the asset or use a supported format.",
            ErrorCode::RenderIo => "Check the render output path and permissions.",
            ErrorCode::RenderStrict => {
                "Strict rendering rejected a fidelity loss. Fix the unsupported draw operation or relax strict mode explicitly."
            }
            ErrorCode::Internal => {
                "ACOS harness failure, not app behaviour. Capture request/scenario/artifacts and report it; do not count it as a pass."
            }
        }
    }

    /// Whether retrying the *same* request without changing inputs may succeed.
    ///
    /// A `true` value never means the failure is benign; it only means the agent
    /// may retry after waiting/addressing the hint.
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            ErrorCode::NoFreshFrame
                | ErrorCode::AppNotResponding
                | ErrorCode::SessionBusy
                | ErrorCode::RamGuard
                | ErrorCode::RamAbort
                | ErrorCode::Timeout
                | ErrorCode::ResourceLimit
                | ErrorCode::RenderIo
                | ErrorCode::NativeIoError
                | ErrorCode::ApkIo
        )
    }

    /// Parse a wire code. Returns `None` for codes this build does not know.
    pub fn from_str_opt(s: &str) -> Option<ErrorCode> {
        ALL_ERROR_CODES
            .iter()
            .copied()
            .find(|code| code.as_str() == s)
    }

    /// The conventional status for this code.
    ///
    /// `Envelope` enforces this mapping so a runtime cannot report
    /// `UNSUPPORTED_*` as `pass`, or an app crash as an ordinary failure.
    pub const fn default_status(self) -> crate::envelope::Status {
        use crate::envelope::Status;
        match self {
            ErrorCode::AssertionFailed | ErrorCode::ElementNotFound => Status::Fail,
            ErrorCode::AppCrashed => Status::Crash,
            ErrorCode::AppNotResponding | ErrorCode::Timeout => Status::Timeout,
            ErrorCode::SessionBusy
            | ErrorCode::ResourceLimit
            | ErrorCode::RamGuard
            | ErrorCode::RamAbort
            | ErrorCode::IsolationUnavailable
            | ErrorCode::IsolationUnsatisfied
            | ErrorCode::ConfinementUnavailable
            | ErrorCode::LandlockUnavailable => Status::Blocked,
            ErrorCode::UnsupportedAndroidApi
            | ErrorCode::UnsupportedJavaApi
            | ErrorCode::UnsupportedBytecode
            | ErrorCode::UnsupportedNativeSymbol
            | ErrorCode::NeedsRealDevice
            | ErrorCode::RequiresGms
            | ErrorCode::WrongAbi
            | ErrorCode::UnsupportedAbi
            | ErrorCode::NoDisplay
            | ErrorCode::NoText
            | ErrorCode::UnsupportedRelocation
            | ErrorCode::RenderUnsupportedOp
            | ErrorCode::UnsupportedBackend
            | ErrorCode::UnsupportedRn
            | ErrorCode::UnsupportedRnSetup
            | ErrorCode::UnsupportedRnDriver
            | ErrorCode::UnsupportedRnNoRenderer
            | ErrorCode::UnsupportedRnKey
            | ErrorCode::UnsupportedRnText
            | ErrorCode::UnsupportedRnNoTarget => Status::Unsupported,
            _ => Status::Error,
        }
    }

    /// True for codes that mean "the engine cannot do this", not "the app is wrong".
    pub const fn is_unsupported(self) -> bool {
        matches!(self.default_status(), crate::envelope::Status::Unsupported)
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ErrorCode {
    type Err = UnknownErrorCode;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ErrorCode::from_str_opt(s).ok_or_else(|| UnknownErrorCode(s.to_string()))
    }
}

/// A wire code that this protocol version does not know.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown ACOS error code: {0}")]
pub struct UnknownErrorCode(pub String);

/// A structured error: stable code, human message, next action, retryability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProtocolError {
    /// The stable machine code.
    pub code: ErrorCode,
    /// Human-readable detail (may change; never branch on it).
    pub message: String,
    /// What the agent should do next. Filled from [`ErrorCode::hint`] by default.
    pub hint: String,
    /// Whether retrying the same request without changing inputs may succeed.
    pub retryable: bool,
    /// Optional structured detail (API/symbol/opcode/path/step index, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl ProtocolError {
    /// Build an error with the code's default hint/retryability.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ProtocolError {
            code,
            message: message.into(),
            hint: code.hint().to_string(),
            retryable: code.retryable(),
            details: None,
        }
    }

    /// Override the default hint with situation-specific guidance.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    /// Attach structured detail for machine-side triage.
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    /// The status this error maps to.
    pub fn status(&self) -> crate::envelope::Status {
        self.code.default_status()
    }
}

/// Every code in this protocol version, in a stable order.
pub const ALL_ERROR_CODES: &[ErrorCode] = &[
    ErrorCode::UnsupportedAndroidApi,
    ErrorCode::UnsupportedJavaApi,
    ErrorCode::UnsupportedBytecode,
    ErrorCode::UnsupportedNativeSymbol,
    ErrorCode::NeedsRealDevice,
    ErrorCode::RequiresGms,
    ErrorCode::WrongAbi,
    ErrorCode::UnsupportedAbi,
    ErrorCode::NoDisplay,
    ErrorCode::NoText,
    ErrorCode::UnsupportedRelocation,
    ErrorCode::RenderUnsupportedOp,
    ErrorCode::UnsupportedBackend,
    ErrorCode::UnsupportedRn,
    ErrorCode::UnsupportedRnSetup,
    ErrorCode::UnsupportedRnDriver,
    ErrorCode::UnsupportedRnNoRenderer,
    ErrorCode::UnsupportedRnKey,
    ErrorCode::UnsupportedRnText,
    ErrorCode::UnsupportedRnNoTarget,
    ErrorCode::NoFreshFrame,
    ErrorCode::UnverifiedFidelity,
    ErrorCode::ApkNotZip,
    ErrorCode::ApkNoManifest,
    ErrorCode::ApkIo,
    ErrorCode::ApkEntry,
    ErrorCode::ApkSigningMalformed,
    ErrorCode::AxmlMalformed,
    ErrorCode::ArscMalformed,
    ErrorCode::DexBadMagic,
    ErrorCode::DexUnsupportedVersion,
    ErrorCode::DexBadChecksum,
    ErrorCode::DexBadSignature,
    ErrorCode::DexTruncated,
    ErrorCode::DexBadEndian,
    ErrorCode::DexIndexOutOfRange,
    ErrorCode::DexBadInstruction,
    ErrorCode::DexUnusedOpcode,
    ErrorCode::DexBadLeb128,
    ErrorCode::DexBadMutf8,
    ErrorCode::DexBadEncodedValue,
    ErrorCode::DexBadClassData,
    ErrorCode::DexBadCodeItem,
    ErrorCode::DexBadDebugInfo,
    ErrorCode::DexBadAnnotation,
    ErrorCode::DexBadMap,
    ErrorCode::DexBadHeader,
    ErrorCode::DexVerifyFailed,
    ErrorCode::DexOverflow,
    ErrorCode::DexLoadError,
    ErrorCode::ElfMalformed,
    ErrorCode::NativeDepNotFound,
    ErrorCode::NativeInitFailed,
    ErrorCode::NativeIoError,
    ErrorCode::NativeSymbolNotFound,
    ErrorCode::NativeInvalidArgument,
    ErrorCode::VmInternal,
    ErrorCode::JavaException,
    ErrorCode::AppCrashed,
    ErrorCode::AppNotResponding,
    ErrorCode::AssertionFailed,
    ErrorCode::ElementNotFound,
    ErrorCode::StaleRef,
    ErrorCode::SessionNotFound,
    ErrorCode::SessionBusy,
    ErrorCode::IsolationUnavailable,
    ErrorCode::ConfinementUnavailable,
    ErrorCode::LandlockUnavailable,
    ErrorCode::IsolationUnsatisfied,
    ErrorCode::RamGuard,
    ErrorCode::RamAbort,
    ErrorCode::Timeout,
    ErrorCode::ResourceLimit,
    ErrorCode::InvalidScenario,
    ErrorCode::InvalidArgument,
    ErrorCode::UnsafePath,
    ErrorCode::ZipSlip,
    ErrorCode::ArtifactsInvalid,
    ErrorCode::RenderBadHandle,
    ErrorCode::RenderFontNotFound,
    ErrorCode::RenderInvalidArgument,
    ErrorCode::RenderSize,
    ErrorCode::RenderDecode,
    ErrorCode::RenderIo,
    ErrorCode::RenderStrict,
    ErrorCode::Internal,
];

/// Codes emitted by existing ACOS crates (verified by grep of `fn code`).
///
/// Kept as strings so this crate does not depend on the runtime crates. A test
/// asserts every one maps to an [`ErrorCode`].
pub const EXISTING_CRATE_CODES: &[&str] = &[
    // acos-apk
    "APK_NOT_ZIP",
    "APK_NO_MANIFEST",
    "APK_IO",
    "AXML_MALFORMED",
    "ARSC_MALFORMED",
    "APK_ENTRY",
    "APK_SIGNING_MALFORMED",
    "UNSUPPORTED_ABI",
    "ZIP_SLIP",
    // acos-dex
    "DEX_BAD_MAGIC",
    "DEX_UNSUPPORTED_VERSION",
    "DEX_BAD_CHECKSUM",
    "DEX_BAD_SIGNATURE",
    "DEX_TRUNCATED",
    "DEX_BAD_ENDIAN",
    "DEX_INDEX_OUT_OF_RANGE",
    "DEX_BAD_INSTRUCTION",
    "DEX_UNUSED_OPCODE",
    "DEX_BAD_LEB128",
    "DEX_BAD_MUTF8",
    "DEX_BAD_ENCODED_VALUE",
    "DEX_BAD_CLASS_DATA",
    "DEX_BAD_CODE_ITEM",
    "DEX_BAD_DEBUG_INFO",
    "DEX_BAD_ANNOTATION",
    "DEX_BAD_MAP",
    "DEX_BAD_HEADER",
    "DEX_VERIFY_FAILED",
    "DEX_OVERFLOW",
    // acos-vm
    "UNSUPPORTED_JAVA_API",
    "UNSUPPORTED_BYTECODE",
    "VM_INTERNAL",
    "DEX_LOAD_ERROR",
    "JAVA_EXCEPTION",
    // acos-native-loader
    "ELF_MALFORMED",
    "WRONG_ABI",
    "NATIVE_DEP_NOT_FOUND",
    "NATIVE_INIT_FAILED",
    "UNSUPPORTED_NATIVE_SYMBOL",
    "UNSUPPORTED_RELOCATION",
    "NATIVE_IO_ERROR",
    "NATIVE_SYMBOL_NOT_FOUND",
    "NATIVE_INVALID_ARGUMENT",
    "CONFINEMENT_UNAVAILABLE",
    // acos-render
    "RENDER_UNSUPPORTED_OP",
    "RENDER_BAD_HANDLE",
    "RENDER_FONT_NOT_FOUND",
    "NO_FRESH_FRAME",
    "RENDER_INVALID_ARGUMENT",
    "RENDER_SIZE",
    "RENDER_DECODE",
    "RENDER_IO",
    "RENDER_STRICT",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_codes_are_unique_and_round_trip() {
        let mut seen = std::collections::BTreeSet::new();
        for &code in ALL_ERROR_CODES {
            assert!(seen.insert(code.as_str()), "duplicate code {}", code);
            assert_eq!(ErrorCode::from_str_opt(code.as_str()), Some(code));
            assert!(!code.hint().is_empty(), "{} has no hint", code);
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
            let back: ErrorCode = serde_json::from_str(&json).unwrap();
            assert_eq!(back, code);
        }
    }

    #[test]
    fn existing_crate_codes_are_all_represented() {
        for name in EXISTING_CRATE_CODES {
            let code = ErrorCode::from_str_opt(name)
                .unwrap_or_else(|| panic!("existing crate code {name} is not in ErrorCode"));
            assert_eq!(code.as_str(), *name);
        }
    }

    #[test]
    fn error_object_carries_hint_and_retryable() {
        let err = ProtocolError::new(ErrorCode::UnsupportedJavaApi, "java.lang.reflect.Proxy");
        assert_eq!(err.status(), crate::envelope::Status::Unsupported);
        assert!(!err.retryable);
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], "UNSUPPORTED_JAVA_API");
        assert!(json["hint"].as_str().unwrap().contains("escalate"));
        assert_eq!(json["retryable"], false);
        let back: ProtocolError = serde_json::from_value(json).unwrap();
        assert_eq!(back, err);
    }
}
