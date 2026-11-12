//! Trust classification for a run and the fail-closed guard it installs.
//!
//! The sandbox is **CERTIFIED-WITH-LIMITS** for untrusted APKs
//! (`docs/SANDBOX_CERTIFICATION.md` §8.6, limits in §8.7), and the owner's
//! product policy (2026-11-12) is **opt-in untrusted**: a run is trusted by
//! default (our own fixtures) and `--untrusted` or a scenario
//! `trust: untrusted` explicitly opts into untrusted mode. There is no
//! auto-detection, and an explicit opt-in is allowed without any environment
//! variable (§8.8).
//!
//! `ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1` remains an accepted, now-redundant alias:
//! existing hostile-fixture tests and scripts keep working, and setting it
//! changes nothing.
//!
//! In untrusted mode the tree, logs and markers are guest-asserted: tree-only /
//! log-only assertions fail closed with `INSUFFICIENT_HOST_EVIDENCE`. Tap-target
//! resolution (a selector that becomes a screen point) still uses the tree for
//! navigation only; it never turns into a pass on its own.
//!
//! `UNTRUSTED_APK_NOT_CERTIFIED` stays registered but is only produced if a
//! future certification downgrade changes [`CERTIFICATION`] to
//! [`Certification::NotCertified`]; see `untrusted_gate`.

use std::path::Path;

use acos_protocol::commands::ResetScope;
use acos_protocol::{Backend as ProtocolBackend, Envelope, ErrorCode};
use acos_verify::backend::{
    Backend, BackendError, Capabilities, EnvironmentChange, InputAction, LaunchOutcome,
    LaunchRequest, LogRecord, ManifestInfo, SessionObservation,
};
use acos_verify::Fidelity;

/// The historical test override. Accepted, now redundant (product policy §8.8).
pub const ALLOW_UNTRUSTED_ENV: &str = "ACOS_ALLOW_UNTRUSTED_FOR_TESTS";

/// Document that names the limits of the certification.
pub const LIMITS_DOC: &str = "docs/SANDBOX_CERTIFICATION.md";
/// Section of [`LIMITS_DOC`] that names the limits.
pub const LIMITS_SECTION: &str = "§8.7";
/// Section of [`LIMITS_DOC`] that states the product policy.
pub const POLICY_SECTION: &str = "§8.8";

/// The product-path certification this build ships with.
///
/// This is the single switch for the untrusted-APK policy. Today it is
/// [`Certification::CertifiedWithLimits`], so an explicit `--untrusted` is
/// allowed. A future re-audit that withdraws the verdict sets this constant to
/// [`Certification::NotCertified`] and `UNTRUSTED_APK_NOT_CERTIFIED` becomes
/// live again with no other code change.
pub const CERTIFICATION: Certification = Certification::CertifiedWithLimits;

/// The product-path verdict for untrusted APKs (`docs/SANDBOX_CERTIFICATION.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Certification {
    /// Certified for untrusted APKs, with the named limits (§8.7).
    CertifiedWithLimits,
    /// A downgrade: untrusted APKs are refused again (the S8 behaviour).
    ///
    /// Nothing constructs this today; it exists so a future re-audit can flip
    /// [`CERTIFICATION`] without any other code change.
    #[allow(dead_code)]
    NotCertified,
}

impl Certification {
    /// Stable string carried by every verdict and envelope.
    pub const fn label(self) -> &'static str {
        match self {
            Certification::CertifiedWithLimits => "certified-with-limits",
            Certification::NotCertified => "not-certified",
        }
    }

    /// Whether an explicit untrusted run may proceed under this verdict.
    pub const fn allows_untrusted(self) -> bool {
        matches!(self, Certification::CertifiedWithLimits)
    }
}

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

/// Whether the historical test override alias is set.
///
/// It gates nothing: an explicit untrusted run is allowed by product policy
/// (`CERTIFICATION`), so the alias is redundant. It is still accepted and
/// recorded so that existing hostile-fixture tests and scripts are unchanged.
pub fn test_override_set() -> bool {
    std::env::var(ALLOW_UNTRUSTED_ENV).ok().as_deref() == Some("1")
}

/// The refusal that only a certification downgrade can produce.
///
/// This is the sole producer of `UNTRUSTED_APK_NOT_CERTIFIED` in the tree. With
/// the shipped [`CERTIFICATION`] it is unreachable; the unit test
/// `certification_downgrade_restores_the_refusal` passes
/// [`Certification::NotCertified`] to prove the flag flips the behaviour.
pub fn refusal_error(trust: Trust, certification: Certification) -> acos_protocol::ProtocolError {
    acos_protocol::ProtocolError::new(
        ErrorCode::UntrustedApkNotCertified,
        format!(
            "untrusted APK runs are refused: the sandbox certification is {} ({} {}); \
             the CERTIFICATION flag in crates/acos-cli/src/trust.rs withdrew the verdict",
            certification.label(),
            LIMITS_DOC,
            POLICY_SECTION
        ),
    )
    .with_hint(format!(
        "Run a trusted fixture. An untrusted run is allowed only while CERTIFICATION is \
         CertifiedWithLimits; see {LIMITS_DOC} {LIMITS_SECTION} and {POLICY_SECTION}."
    ))
    .with_details(serde_json::json!({
        "trust": trust.as_str(),
        "certification": certification.label(),
        "certification_limits": limits_pointer(),
        "override_env": ALLOW_UNTRUSTED_ENV,
        "override_set": test_override_set(),
        "override_required": false,
    }))
}

/// The pointer to the limits of the certification.
pub fn limits_pointer() -> serde_json::Value {
    serde_json::json!({
        "doc": LIMITS_DOC,
        "section": LIMITS_SECTION,
    })
}

/// The `trust` / `certification` record every verdict and envelope carries.
///
/// `trust` is `unresolved` only for the pre-classification failure (an invalid
/// scenario `trust:` value); the run was never classified in that case.
pub fn certification_evidence(trust: Option<Trust>) -> serde_json::Value {
    serde_json::json!({
        "trust": trust.map(Trust::as_str).unwrap_or("unresolved"),
        "certification": CERTIFICATION.label(),
        "certification_limits": limits_pointer(),
        "certification_policy": {
            "doc": LIMITS_DOC,
            "section": POLICY_SECTION,
            "policy": "opt-in untrusted: trusted is the default, an explicit --untrusted or `trust: untrusted` is allowed",
        },
    })
}

/// The product gate: `Some(refusal)` only when a downgraded certification blocks
/// an explicitly untrusted run.
pub fn untrusted_gate(
    trust: Trust,
    certification: Certification,
) -> Option<acos_protocol::ProtocolError> {
    if trust == Trust::Untrusted && !certification.allows_untrusted() {
        return Some(refusal_error(trust, certification));
    }
    None
}

/// Put `trust` / `certification` / the limits pointer into the envelope result.
///
/// The verdict gets the same labels through
/// [`crate::provenance::annotate_verdict`]; this covers failures that never
/// produce a verdict (refusals, invalid arguments, engine errors).
pub fn stamp_envelope(envelope: &Envelope, trust: Option<Trust>) -> Envelope {
    let mut result = envelope
        .result()
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(object) = result.as_object_mut() {
        let evidence = certification_evidence(trust);
        object.insert("trust".to_string(), evidence["trust"].clone());
        object.insert(
            "certification".to_string(),
            evidence["certification"].clone(),
        );
        object.insert(
            "certification_limits".to_string(),
            evidence["certification_limits"].clone(),
        );
        object.insert(
            "certification_policy".to_string(),
            evidence["certification_policy"].clone(),
        );
    }
    envelope.clone().with_value_result(result)
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
        let mut caps = self.inner.capabilities();
        if self.trust == Trust::Untrusted {
            // Crash reports and logs are guest-asserted: on an untrusted run
            // they can never justify a pass, so the step is unsupported rather
            // than silently trusted.
            caps.logs = false;
            caps.crash_reports = false;
        }
        caps
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
        if self.trust == Trust::Untrusted {
            // Logs are guest-asserted (the APK decides what it writes). On an
            // untrusted run they can never justify a pass; refuse the channel
            // so `assert_log` fails closed with the precise code instead of
            // matching a forged line.
            return Err(BackendError::Failed {
                code: ErrorCode::InsufficientHostEvidence,
                detail: "UNTRUSTED_LOG_ASSERTION: this run is untrusted, so the guest log is \
                         guest-asserted and cannot justify a pass. Use a host-observed assertion \
                         (assert_region / assert_screenshot / capture)."
                    .to_string(),
            });
        }
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
    fn certification_flag_flips_the_untrusted_gate() {
        // Shipped verdict: an explicit untrusted run is allowed, no env var.
        assert_eq!(CERTIFICATION, Certification::CertifiedWithLimits);
        assert!(untrusted_gate(Trust::Untrusted, CERTIFICATION).is_none());
        // Trusted runs are never gated, whatever the certification says.
        assert!(untrusted_gate(Trust::Trusted, Certification::NotCertified).is_none());
        // A future downgrade restores the S8 refusal with the same code.
        let refusal = untrusted_gate(Trust::Untrusted, Certification::NotCertified)
            .expect("a downgraded certification must refuse an untrusted run");
        assert_eq!(refusal.code, ErrorCode::UntrustedApkNotCertified);
        assert_eq!(refusal.details.as_ref().unwrap()["certification"], "not-certified");
        assert_eq!(
            refusal.details.as_ref().unwrap()["override_required"],
            false
        );
    }

    #[test]
    fn certification_record_names_the_limits() {
        let evidence = certification_evidence(Some(Trust::Untrusted));
        assert_eq!(evidence["trust"], "untrusted");
        assert_eq!(evidence["certification"], "certified-with-limits");
        assert_eq!(evidence["certification_limits"]["doc"], LIMITS_DOC);
        assert_eq!(evidence["certification_limits"]["section"], LIMITS_SECTION);
        assert_eq!(evidence["certification_policy"]["section"], POLICY_SECTION);
        assert_eq!(certification_evidence(None)["trust"], "unresolved");
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
    fn untrusted_logs_fail_closed_with_insufficient_host_evidence() {
        use crate::engine::fake::FakeSession;
        use crate::engine::EngineBackend;

        let inner = EngineBackend::session(Box::new(FakeSession::live()));
        let mut untrusted = TrustGuard::new(Box::new(inner), Trust::Untrusted);
        let error = untrusted
            .logs_since(0)
            .expect_err("an untrusted run must refuse guest-asserted logs");
        assert_eq!(error.code(), ErrorCode::InsufficientHostEvidence);
        assert!(
            error.to_string().contains("UNTRUSTED_LOG_ASSERTION"),
            "{error}"
        );

        let inner = EngineBackend::session(Box::new(FakeSession::live()));
        let mut trusted = TrustGuard::new(Box::new(inner), Trust::Trusted);
        let error = trusted
            .logs_since(0)
            .expect_err("the fake has no log command; the point is the code forwarded");
        assert_eq!(error.code(), ErrorCode::UnsupportedBackend);
    }

    #[test]
    fn unknown_trust_is_an_error_not_a_downgrade() {
        let path = scenario("version: 1\nname: x\ntrust: maybe\napp: a.apk\nsteps: []\n");
        assert!(resolve(false, &path).is_err());
    }
}
