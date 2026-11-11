//! Trust classification for a run and the fail-closed guard it installs.
//!
//! The product path is **not certified for untrusted APKs** yet
//! (`docs/SANDBOX_CERTIFICATION.md` §5/§7). A run is trusted by default (our
//! own fixtures); `--untrusted` or a scenario `trust: untrusted` marks it
//! untrusted. Untrusted runs are refused unless the labelled test override
//! `ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1` is set, which only the hostile-fixture
//! tests use.
//!
//! When an untrusted run is allowed for a test, the tree is guest-asserted:
//! tree-only assertions fail closed with `INSUFFICIENT_HOST_EVIDENCE`. Tap-target
//! resolution (a selector that becomes a screen point) still uses the tree for
//! navigation only; it never turns into a pass on its own.

use std::path::Path;

use acos_protocol::commands::ResetScope;
use acos_protocol::{Backend as ProtocolBackend, ErrorCode};
use acos_verify::backend::{
    Backend, BackendError, Capabilities, EnvironmentChange, InputAction, LaunchOutcome,
    LaunchRequest, LogRecord, ManifestInfo, SessionObservation,
};
use acos_verify::Fidelity;

/// The override that only the hostile-fixture tests may set.
pub const ALLOW_UNTRUSTED_ENV: &str = "ACOS_ALLOW_UNTRUSTED_FOR_TESTS";

/// How much the run can trust guest-reported evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// Our own fixtures: guest-asserted evidence may corroborate a pass.
    Trusted,
    /// Third-party/hostile input: only host-observed evidence may justify a pass.
    Untrusted,
}

impl Trust {
    /// Stable verdict string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Trust::Trusted => "trusted",
            Trust::Untrusted => "untrusted",
        }
    }
}

/// Resolve trust from `--untrusted` and the scenario's top-level `trust:` key.
///
/// The flag wins. An unknown scenario value is an error, never a silent
/// downgrade to trusted.
pub fn resolve(flag_untrusted: bool, scenario: &Path) -> Result<Trust, String> {
    if flag_untrusted {
        return Ok(Trust::Untrusted);
    }
    let text = std::fs::read_to_string(scenario)
        .map_err(|error| format!("cannot read scenario {}: {error}", scenario.display()))?;
    for line in text.lines() {
        // Only a top-level key counts; nested `trust:` keys are not part of the
        // schema and must not change the run's classification.
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let Some(value) = line.strip_prefix("trust:") else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        return match value {
            "trusted" => Ok(Trust::Trusted),
            "untrusted" => Ok(Trust::Untrusted),
            other => Err(format!(
                "unknown scenario trust value {other:?}; expected `trusted` or `untrusted`"
            )),
        };
    }
    Ok(Trust::Trusted)
}

/// Whether this process is allowed to run an untrusted APK.
pub fn untrusted_allowed_by_env() -> bool {
    std::env::var(ALLOW_UNTRUSTED_ENV).ok().as_deref() == Some("1")
}

/// The structured refusal for an untrusted run without the test override.
pub fn refusal_error(trust: Trust) -> acos_protocol::ProtocolError {
    acos_protocol::ProtocolError::new(
        ErrorCode::UntrustedApkNotCertified,
        "untrusted APK runs are refused: the product path is not certified for untrusted APKs",
    )
    .with_hint(format!(
        "Run a trusted fixture, or set {ALLOW_UNTRUSTED_ENV}=1 for a labelled hostile-fixture test only. \
         Never enable this for an arbitrary APK; see docs/SANDBOX_CERTIFICATION.md §5/§7."
    ))
    .with_details(serde_json::json!({
        "trust": trust.as_str(),
        "override_env": ALLOW_UNTRUSTED_ENV,
        "override_set": untrusted_allowed_by_env(),
        "certification": "NOT_CERTIFIED_FOR_UNTRUSTED_APKS",
    }))
}

/// A backend wrapper that fails closed on guest-asserted tree observations for
/// an untrusted run.
///
/// `capture` (host pixels), `input` (host-injected, with tap-target resolution
/// from the tree for navigation only) and every session/launch operation pass
/// through unchanged. Only `observe` — the call every tree-only assertion makes
/// — is refused with `INSUFFICIENT_HOST_EVIDENCE`.
pub struct TrustGuard {
    inner: Box<dyn Backend>,
    trust: Trust,
}

impl TrustGuard {
    /// Wrap a backend at the requested trust level.
    pub fn new(inner: Box<dyn Backend>, trust: Trust) -> Self {
        TrustGuard { inner, trust }
    }
}

impl Backend for TrustGuard {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn protocol_backend(&self) -> ProtocolBackend {
        self.inner.protocol_backend()
    }

    fn fidelity(&self) -> Fidelity {
        self.inner.fidelity()
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn manifest(&mut self, apk: &Path) -> Result<ManifestInfo, BackendError> {
        self.inner.manifest(apk)
    }

    fn launch(&mut self, request: &LaunchRequest) -> Result<LaunchOutcome, BackendError> {
        self.inner.launch(request)
    }

    fn observe(&mut self) -> Result<SessionObservation, BackendError> {
        if self.trust == Trust::Untrusted {
            return Err(BackendError::Failed {
                code: ErrorCode::InsufficientHostEvidence,
                detail: "UNTRUSTED_TREE_ASSERTION: this run is untrusted, so the UI tree is \
                         guest-asserted and cannot justify a pass. Use a host-observed assertion \
                         (assert_region / assert_screenshot / capture); the tree is still used to \
                         resolve tap targets for navigation."
                    .to_string(),
            });
        }
        self.inner.observe()
    }

    fn logs_since(&mut self, since: u64) -> Result<Vec<LogRecord>, BackendError> {
        self.inner.logs_since(since)
    }

    fn input(&mut self, action: &InputAction) -> Result<SessionObservation, BackendError> {
        // Allowed in untrusted mode: the host injected the input. A tap with a
        // selector resolves its point from the tree (navigation only).
        self.inner.input(action)
    }

    fn reset(&mut self, scope: ResetScope) -> Result<(), BackendError> {
        self.inner.reset(scope)
    }

    fn configure(&mut self, change: &EnvironmentChange) -> Result<(), BackendError> {
        self.inner.configure(change)
    }

    fn capture(&mut self) -> Result<SessionObservation, BackendError> {
        self.inner.capture()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario(text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acos-trust-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scenario.yaml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn default_is_trusted_and_untrusted_is_explicit() {
        let path = scenario("version: 1\nname: x\napp: a.apk\nsteps: []\n");
        assert_eq!(resolve(false, &path).unwrap(), Trust::Trusted);
        let path = scenario("version: 1\nname: x\ntrust: untrusted\napp: a.apk\nsteps: []\n");
        assert_eq!(resolve(false, &path).unwrap(), Trust::Untrusted);
        assert_eq!(resolve(true, &path).unwrap(), Trust::Untrusted);
    }

    #[test]
    fn nested_trust_key_does_not_change_the_run() {
        let path = scenario(
            "version: 1\nname: x\napp: a.apk\nsteps:\n  - tap:\n      x: 1\n      y: 2\n      note: \"trust: untrusted\"\n",
        );
        assert_eq!(resolve(false, &path).unwrap(), Trust::Trusted);
    }

    #[test]
    fn unknown_trust_is_an_error_not_a_downgrade() {
        let path = scenario("version: 1\nname: x\ntrust: maybe\napp: a.apk\nsteps: []\n");
        assert!(resolve(false, &path).is_err());
    }
}
