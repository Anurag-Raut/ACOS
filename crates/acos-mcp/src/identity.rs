//! Daemon/client build identity (REL3D).
//!
//! `acos serve` is a **shared** daemon: one socket under the runtime directory
//! serves every `acos` invocation of that user, and the MCP server's private
//! daemon is the same code in another process. Before this module, nothing in
//! the wire protocol said *which build* the daemon was. MODA11Y: an older
//! daemon kept answering a newer client binary with its own (stale) behaviour —
//! `code 5 unsupported` for a service the newer build implements. For an agent
//! that is a wrong answer produced by code it never ran, and the owner's
//! installed release hits the same thing after an upgrade.
//!
//! So every daemon reports an [`Identity`] in `hello`/`ping`, every client sends
//! its own, and the comparison happens **before any request** ([`DaemonClient`]
//! refuses a mismatched daemon; the CLI/MCP routing layers then replace an idle
//! auto-started daemon or start a build-private one).
//!
//! The identity is:
//!
//! * `version` — the workspace crate version;
//! * `git_sha` — the checkout's `HEAD` at build time (`unknown` without git);
//! * `source_hash` — a hash of every workspace source/manifest (identical for
//!   all binaries of one build, different for any other code);
//! * `exe`/`exe_len`/`exe_hash` — the running binary itself, hashed **once** at
//!   first use. Two processes running the same path with different bytes are
//!   different builds even when the version string and sources agree (an
//!   in-place upgrade of `~/.local/bin/acos` under a live daemon, a rebuilt
//!   `target/release/acos`);
//! * `build_id` — `"<git_sha>.<source8>.<exe8>"`, the one string to quote.
//!
//! Every field is computed locally and compared, not trusted from a config.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Wire-protocol generation of the daemon control surface (`hello`, identity).
///
/// A daemon that predates it has no `hello` tool at all, which is itself a
/// mismatch ([`DaemonIdentity::Unknown`] on the client side).
pub const PROTOCOL: u32 = 1;

/// The control tool that exchanges identities. Answered by every daemon before
/// any session tool runs.
pub const TOOL_HELLO: &str = "hello";

/// The structured warning emitted when a client refuses a daemon (never a
/// silent substitution): `code` is the stable wire code of the same name.
pub const WARNING_CODE: &str = "DAEMON_VERSION_MISMATCH";

/// The build's git revision (see `build.rs`).
pub const GIT_SHA: &str = env!("ACOS_BUILD_GIT_SHA");
/// The build's workspace source digest (see `build.rs`).
pub const SOURCE_HASH: &str = env!("ACOS_BUILD_SOURCE_HASH");
/// Whether the build was made from a dirty tree (see `build.rs`).
pub const DIRTY: &str = env!("ACOS_BUILD_DIRTY");
/// The workspace crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The identity of one ACOS process (client or daemon).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// [`PROTOCOL`].
    #[serde(default)]
    pub protocol: u32,
    /// The workspace crate version.
    pub version: String,
    /// `HEAD` at build time (`unknown` when the binary was built without git).
    pub git_sha: String,
    /// Digest of the workspace sources the binary was built from.
    #[serde(default)]
    pub source_hash: String,
    /// `"<git_sha>.<source8>.<exe8>"`: the string to quote in a report.
    pub build_id: String,
    /// The running binary's path.
    #[serde(default)]
    pub exe: String,
    /// Its length in bytes (`0` when unreadable).
    #[serde(default)]
    pub exe_len: u64,
    /// FNV-1a of its contents (`unknown` when unreadable).
    #[serde(default)]
    pub exe_hash: String,
    /// The reporting process's pid.
    #[serde(default)]
    pub pid: u32,
    /// The daemon's default engine (`None` for a client).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Whether a client auto-started this daemon (`acos session …`, a private
    /// MCP daemon) rather than an operator running `acos serve`. Only an
    /// auto-started, idle daemon may be replaced on a version mismatch.
    #[serde(default)]
    pub auto_started: bool,
}

impl Identity {
    /// This process's identity, computed once.
    #[must_use]
    pub fn local() -> &'static Identity {
        static LOCAL: OnceLock<Identity> = OnceLock::new();
        LOCAL.get_or_init(|| Self::for_binary(std::env::current_exe().ok()))
    }

    /// This process's identity as a daemon serving `engine`.
    #[must_use]
    pub fn daemon(engine: &str, auto_started: bool) -> Identity {
        let mut identity = Self::local().clone();
        identity.engine = Some(engine.to_string());
        identity.auto_started = auto_started;
        identity
    }

    /// Build an identity for `exe` (the testable form of [`Identity::local`]).
    #[must_use]
    pub fn for_binary(exe: Option<PathBuf>) -> Identity {
        let exe = exe.unwrap_or_default();
        let (len, hash) = if exe.as_os_str().is_empty() {
            (0, "unknown".to_string())
        } else {
            hash_file(&exe)
        };
        let short = |text: &str, n: usize| -> String { text.chars().take(n).collect() };
        Identity {
            protocol: PROTOCOL,
            version: VERSION.to_string(),
            git_sha: GIT_SHA.to_string(),
            source_hash: SOURCE_HASH.to_string(),
            build_id: format!(
                "{}.{}.{}",
                if DIRTY == "dirty" {
                    format!("{GIT_SHA}+dirty")
                } else {
                    GIT_SHA.to_string()
                },
                short(SOURCE_HASH, 8),
                short(&hash, 8)
            ),
            exe: exe.to_string_lossy().into_owned(),
            exe_len: len,
            exe_hash: hash,
            pid: std::process::id(),
            engine: None,
            auto_started: false,
        }
    }

    /// The one-line human identity: `version git build_id`.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} {} {}", self.version, self.git_sha, self.build_id)
    }

    /// Why this client may not use `daemon`, or `None` when they are the same
    /// build (and, when they run the same path, the same binary).
    ///
    /// The rules, in order:
    ///
    /// 1. `protocol` — a different control-surface generation;
    /// 2. `version` — a different workspace version;
    /// 3. `build` — a different git revision or source digest (two binaries of
    ///    one build agree on both, so an `acos-mcp` server and the `acos` daemon
    ///    it spawns agree too);
    /// 4. `binary` — the **same path** with different bytes or length: the file
    ///    was replaced or rebuilt in place while the daemon kept the old image
    ///    (the installed-release upgrade case).
    #[must_use]
    pub fn mismatch(&self, daemon: &Identity) -> Option<MismatchReason> {
        if self.protocol != daemon.protocol {
            return Some(MismatchReason::Protocol {
                client: self.protocol,
                daemon: daemon.protocol,
            });
        }
        if self.version != daemon.version {
            return Some(MismatchReason::Version {
                client: self.version.clone(),
                daemon: daemon.version.clone(),
            });
        }
        // `unknown` git sha (a build without git) cannot discriminate; the
        // source digest and the binary hash still can.
        if self.git_sha != "unknown"
            && daemon.git_sha != "unknown"
            && self.git_sha != daemon.git_sha
        {
            return Some(MismatchReason::Build {
                client: self.label(),
                daemon: daemon.label(),
            });
        }
        if !self.source_hash.is_empty()
            && !daemon.source_hash.is_empty()
            && self.source_hash != daemon.source_hash
        {
            return Some(MismatchReason::Build {
                client: self.label(),
                daemon: daemon.label(),
            });
        }
        // Two different binaries of one build (acos vs acos-mcp) are expected;
        // the same path with different bytes is not.
        if same_path(&self.exe, &daemon.exe)
            && self.exe_hash != "unknown"
            && daemon.exe_hash != "unknown"
            && (self.exe_len != daemon.exe_len || self.exe_hash != daemon.exe_hash)
        {
            return Some(MismatchReason::Binary {
                path: self.exe.clone(),
                client: format!("len={} hash={}", self.exe_len, self.exe_hash),
                daemon: format!("len={} hash={}", daemon.exe_len, daemon.exe_hash),
            });
        }
        None
    }
}

/// Two path spellings of the same executable (`current_exe` already resolves
/// `/proc/self/exe`, but a caller may have been given a symlink).
fn same_path(left: &str, right: &str) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    if left == right {
        return true;
    }
    match (Path::new(left).canonicalize(), Path::new(right).canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// FNV-1a over the file's contents, streaming (a 60 MB binary never lands in
/// memory) plus its length.
fn hash_file(path: &Path) -> (u64, String) {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut len: u64 = 0;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return (0, "unknown".to_string()),
    };
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                len += n as u64;
                for byte in &buffer[..n] {
                    hash ^= u64::from(*byte);
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
            Err(_) => return (0, "unknown".to_string()),
        }
    }
    (len, format!("{hash:016x}"))
}

/// The first rule [`Identity::mismatch`] found broken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MismatchReason {
    /// Different identity protocol generation.
    Protocol { client: u32, daemon: u32 },
    /// Different crate version.
    Version { client: String, daemon: String },
    /// Different build (git revision / source digest).
    Build { client: String, daemon: String },
    /// Same binary path, different bytes (replaced or rebuilt in place).
    Binary {
        path: String,
        client: String,
        daemon: String,
    },
}

impl MismatchReason {
    /// A stable short rule name (`protocol`, `version`, `build`, `binary`).
    #[must_use]
    pub fn rule(&self) -> &'static str {
        match self {
            MismatchReason::Protocol { .. } => "protocol",
            MismatchReason::Version { .. } => "version",
            MismatchReason::Build { .. } => "build",
            MismatchReason::Binary { .. } => "binary",
        }
    }
}

/// What the client learned about the daemon it reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonIdentity {
    /// The daemon reported an identity (`hello`).
    Known(Identity),
    /// The daemon has no `hello` tool or refused it: an older build. The reason
    /// is the transport/daemon error it answered with; `pid` comes from its
    /// `ping` (the one status probe an older daemon still answers), so the
    /// client can tell whether the process may be replaced.
    Unknown {
        reason: String,
        pid: Option<u32>,
    },
}

impl DaemonIdentity {
    /// The reported identity, if any.
    #[must_use]
    pub fn known(&self) -> Option<&Identity> {
        match self {
            DaemonIdentity::Known(identity) => Some(identity),
            DaemonIdentity::Unknown { .. } => None,
        }
    }

    /// The daemon's pid, when it reported one (`hello`, or `ping` for a daemon
    /// that predates the identity protocol).
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        match self {
            DaemonIdentity::Known(identity) => Some(identity.pid),
            DaemonIdentity::Unknown { pid, .. } => *pid,
        }
    }

    /// As a JSON value for a report/envelope (both arms are structured).
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            DaemonIdentity::Known(identity) => json!({
                "identity": identity,
                "predates_identity_protocol": false,
            }),
            DaemonIdentity::Unknown { reason, pid } => json!({
                "identity": null,
                "predates_identity_protocol": true,
                "pid": pid,
                "reason": reason,
            }),
        }
    }
}

/// A refused daemon: the client's identity, the daemon's, and the rule.
///
/// This is the `DAEMON_VERSION_MISMATCH` warning body (CLI stderr, MCP stderr,
/// envelope `error.details`, `acos doctor`).
#[derive(Debug, Clone)]
pub struct Mismatch {
    pub reason: MismatchReason,
    pub client: Identity,
    pub daemon: DaemonIdentity,
}

impl Mismatch {
    /// The stable warning/error code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        WARNING_CODE
    }

    /// The human one-liner: which rule broke and both identities.
    #[must_use]
    pub fn message(&self) -> String {
        let daemon = match &self.daemon {
            DaemonIdentity::Known(identity) => identity.label(),
            DaemonIdentity::Unknown { reason, pid } => format!(
                "predates the identity protocol (pid {}, {reason})",
                pid.map(|pid| pid.to_string()).unwrap_or_else(|| "?".to_string())
            ),
        };
        format!(
            "refusing the running `acos serve` daemon: {} (client {} / daemon {daemon})",
            self.reason.rule(),
            self.client.label()
        )
    }

    /// The structured warning object (`code`, both identities, the rule).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "code": WARNING_CODE,
            "severity": "warning",
            "rule": self.reason.rule(),
            "message": self.message(),
            "client": self.client,
            "daemon": self.daemon.to_json(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(build: &str, exe: &str, hash: &str) -> Identity {
        let mut identity = Identity::local().clone();
        identity.git_sha = "0123456789ab".into();
        identity.source_hash = build.to_string();
        identity.build_id = format!("0123456789ab.{build}.{hash}");
        identity.exe = exe.to_string();
        identity.exe_len = if hash == "unknown" { 0 } else { 4096 };
        identity.exe_hash = hash.to_string();
        identity
    }

    #[test]
    fn the_local_identity_names_the_build_and_the_binary() {
        let local = Identity::local();
        assert_eq!(local.protocol, PROTOCOL);
        assert_eq!(local.version, VERSION);
        assert_eq!(local.pid, std::process::id());
        assert!(!local.build_id.is_empty());
        assert!(local.build_id.contains('.'));
        assert_ne!(local.exe_hash, "unknown", "the test binary is readable");
        assert!(local.exe_len > 0);
        // Same process, same answer (computed once).
        assert_eq!(local, Identity::local());
        // A daemon identity additionally names its engine and provenance.
        let daemon = Identity::daemon("art", true);
        assert_eq!(daemon.engine.as_deref(), Some("art"));
        assert!(daemon.auto_started);
        assert_eq!(daemon.build_id, local.build_id);
        assert_eq!(local.mismatch(&daemon), None, "same build, same binary");
    }

    #[test]
    fn a_different_build_is_a_build_mismatch_even_at_another_path() {
        let client = identity("aaaa1111", "/builds/new/acos", "00000000deadbeef");
        let daemon = identity("bbbb2222", "/builds/old/acos", "11111111deadbeef");
        let reason = client.mismatch(&daemon).expect("different source digest");
        assert_eq!(reason.rule(), "build");
        assert!(matches!(reason, MismatchReason::Build { .. }));

        // A different git revision alone is a build mismatch too.
        let mut other = daemon.clone();
        other.source_hash = client.source_hash.clone();
        other.git_sha = "ffffffffffff".into();
        assert_eq!(
            client.mismatch(&other).map(|r| r.rule()),
            Some("build"),
            "a different HEAD is a different build"
        );
    }

    #[test]
    fn the_same_path_with_different_bytes_is_a_binary_mismatch() {
        // The installed-release upgrade: `~/.local/bin/acos` was replaced while
        // the old daemon kept running the deleted image.
        let client = identity(SOURCE_HASH, "/home/u/.local/bin/acos", "aaaa1111");
        let daemon = identity(SOURCE_HASH, "/home/u/.local/bin/acos", "bbbb2222");
        let reason = client.mismatch(&daemon).expect("same path, other bytes");
        assert_eq!(reason.rule(), "binary");
        // Two *different* binaries of one build (acos vs acos-mcp) are fine.
        let mut other = daemon.clone();
        other.exe = "/home/u/.local/bin/acos-mcp".into();
        assert_eq!(client.mismatch(&other), None);
    }

    #[test]
    fn version_and_protocol_mismatches_name_their_rule() {
        let client = identity(SOURCE_HASH, "/x/acos", "aaaa");
        let mut newer = client.clone();
        newer.version = "9.9.9".into();
        assert_eq!(client.mismatch(&newer).map(|r| r.rule()), Some("version"));
        let mut future = client.clone();
        future.protocol = PROTOCOL + 1;
        assert_eq!(client.mismatch(&future).map(|r| r.rule()), Some("protocol"));
    }

    #[test]
    fn an_unknown_git_sha_or_binary_hash_never_false_matches() {
        let mut client = identity(SOURCE_HASH, "/x/acos", "unknown");
        let mut daemon = client.clone();
        daemon.exe_hash = "aaaa".into();
        daemon.exe_len = 4096;
        // A build without git still discriminates on the source digest.
        assert_eq!(client.mismatch(&daemon), None, "unknown hash cannot compare");
        daemon.source_hash = "00112233".into();
        assert_eq!(client.mismatch(&daemon).map(|r| r.rule()), Some("build"));
        client.git_sha = "unknown".into();
        daemon.git_sha = "0123456789ab".into();
        daemon.source_hash = client.source_hash.clone();
        assert_eq!(client.mismatch(&daemon), None, "git cannot discriminate");
    }

    #[test]
    fn the_warning_is_structured_and_carries_both_identities() {
        let client = identity("aaaa1111", "/builds/new/acos", "00000000deadbeef");
        let daemon = identity("bbbb2222", "/builds/old/acos", "11111111deadbeef");
        let mismatch = Mismatch {
            reason: client.mismatch(&daemon).unwrap(),
            client: client.clone(),
            daemon: DaemonIdentity::Known(daemon),
        };
        let warning = mismatch.to_json();
        assert_eq!(warning["code"], WARNING_CODE);
        assert_eq!(warning["rule"], "build");
        assert_eq!(warning["client"]["build_id"], "0123456789ab.aaaa1111.00000000deadbeef");
        assert_eq!(warning["daemon"]["identity"]["build_id"], "0123456789ab.bbbb2222.11111111deadbeef");
        assert!(warning["message"].as_str().unwrap().contains("client"));

        // A daemon that predates the protocol is still structured, not empty.
        let legacy = mismatch_with_unknown(client);
        let warning = legacy.to_json();
        assert_eq!(warning["daemon"]["identity"], Value::Null);
        assert_eq!(warning["daemon"]["predates_identity_protocol"], true);
        assert!(warning["message"]
            .as_str()
            .unwrap()
            .contains("predates the identity protocol"));
    }

    fn client_only() -> Identity {
        identity("aaaa1111", "/builds/new/acos", "00000000deadbeef")
    }

    fn mismatch_with_unknown(client: Identity) -> Mismatch {
        Mismatch {
            reason: MismatchReason::Build {
                client: client.label(),
                daemon: "unknown".into(),
            },
            client,
            daemon: DaemonIdentity::Unknown {
                reason: "SESSION_NOT_FOUND: no such tool".into(),
                pid: Some(4242),
            },
        }
    }

    #[test]
    fn a_structuring_helper_is_used_by_the_docs_example() {
        let client = client_only();
        let mismatch = mismatch_with_unknown(client.clone());
        assert_eq!(mismatch.code(), "DAEMON_VERSION_MISMATCH");
        assert_eq!(mismatch.client, client);
        assert_eq!(
            DaemonIdentity::Unknown {
                reason: "x".into(),
                pid: Some(7)
            }
            .pid(),
            Some(7)
        );
        assert_eq!(
            DaemonIdentity::Unknown {
                reason: "x".into(),
                pid: None
            }
            .pid(),
            None
        );
        assert_eq!(DaemonIdentity::Known(client.clone()).pid(), Some(client.pid));
    }
}
