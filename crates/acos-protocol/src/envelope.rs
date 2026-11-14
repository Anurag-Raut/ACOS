//! The response envelope shared by every ACOS command.
//!
//! Compatibility note: this envelope is a superset of the object emitted by the
//! existing `acos inspect --json` (and `acos run-dex --json`):
//! `request_id`, `ok`, `status`, `backend`, `fidelity`, `capabilities_used`,
//! `error`, `result`, `metrics`. New fields are optional and omitted when empty.
//!
//! The core invariant is structural: **`ok: true` is only representable with
//! `status: pass`, and a non-pass status must carry an error whose code maps to
//! that status.** Fields are private; only the constructors and a validating
//! `Deserialize` can create an envelope, so a false pass is not representable
//! without editing this crate (and `unsafe` is forbidden).

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::error::ProtocolError;
use crate::frame::FrameMeta;
use crate::identity::IsolationLevel;

/// The outcome of a request.
///
/// `unsupported`, `blocked`, `crash` and `timeout` are failures. An agent must
/// never treat any of them as a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The request ran and every requested check held under the stated fidelity.
    Pass,
    /// The request ran and a correctness check or assertion did not hold.
    Fail,
    /// The harness, engine or input failed; not an app verdict.
    Error,
    /// The engine cannot perform the requested behaviour (never a pass).
    Unsupported,
    /// The app crashed.
    Crash,
    /// The request exceeded a wall/CPU budget or the app is not responding.
    Timeout,
    /// The request cannot start until an external condition is resolved
    /// (resource guard, busy session, unavailable isolation).
    Blocked,
}

impl Status {
    /// True only for [`Status::Pass`].
    pub const fn is_pass(self) -> bool {
        matches!(self, Status::Pass)
    }

    /// True for every non-pass status.
    pub const fn is_failure(self) -> bool {
        !self.is_pass()
    }

    /// The stable wire string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Error => "error",
            Status::Unsupported => "unsupported",
            Status::Crash => "crash",
            Status::Timeout => "timeout",
            Status::Blocked => "blocked",
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which thing actually produced the verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    /// The ACOS Rust runtime (VM, native loader, renderer, UI).
    AcosNative,
    /// The legacy ACVM/AOSP reference backend.
    AcvmReference,
    /// Static host tooling (APK/DEX parsing, lint, build).
    HostTooling,
    /// A host JVM used as a weak semantics oracle.
    Jvm,
    /// A physical Android device.
    PhysicalDevice,
}

impl Backend {
    /// The stable wire string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Backend::AcosNative => "acos-native",
            Backend::AcvmReference => "acvm-reference",
            Backend::HostTooling => "host-tooling",
            Backend::Jvm => "jvm",
            Backend::PhysicalDevice => "physical-device",
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How faithful the execution behind a result is.
///
/// * `static-analysis` — host parsing only. Proves nothing about runtime behaviour.
/// * `jvm-reference` — host OpenJDK. Proves pure-Java semantics only; never
///   Android/ART/framework/JNI/rendering behaviour.
/// * `native-runtime` — executed by ACOS itself (its Rust VM/loader/renderer).
///   This is **not** validated against ART unless `validated_against` says so.
/// * `art-reference` — real ART on a reference Android image. Authoritative for
///   Android framework behaviour at the stated API/ABI.
/// * `physical-device` — a real device/OEM target.
/// * `simulated` — a mock/stub path. A simulated success is not an Android success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Fidelity {
    /// Static host analysis only.
    StaticAnalysis,
    /// Host JVM (weak oracle; never Android-verified).
    JvmReference,
    /// ACOS native execution; not ART-validated by itself.
    NativeRuntime,
    /// Real ART reference capture/execution.
    ArtReference,
    /// Physical device/OEM target.
    PhysicalDevice,
    /// Mocked/simulated behaviour.
    Simulated,
}

impl Fidelity {
    /// The stable wire string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Fidelity::StaticAnalysis => "static-analysis",
            Fidelity::JvmReference => "jvm-reference",
            Fidelity::NativeRuntime => "native-runtime",
            Fidelity::ArtReference => "art-reference",
            Fidelity::PhysicalDevice => "physical-device",
            Fidelity::Simulated => "simulated",
        }
    }

    /// What a pass at this fidelity actually proves.
    pub const fn description(self) -> &'static str {
        match self {
            Fidelity::StaticAnalysis => {
                "host parsing/analysis only; proves nothing about runtime behaviour"
            }
            Fidelity::JvmReference => {
                "host JVM semantics only; not Android, ART, framework, JNI or rendering"
            }
            Fidelity::NativeRuntime => {
                "executed by ACOS itself; not validated against ART unless validated_against says so"
            }
            Fidelity::ArtReference => {
                "executed on real ART; authoritative for the stated API/ABI"
            }
            Fidelity::PhysicalDevice => "executed on a physical device/OEM target",
            Fidelity::Simulated => "mocked/simulated behaviour; not an Android success",
        }
    }

    /// True only for fidelities that directly observe real Android behaviour.
    pub const fn is_android_verified(self) -> bool {
        matches!(self, Fidelity::ArtReference | Fidelity::PhysicalDevice)
    }

    /// True when a pass at this fidelity can satisfy a requirement for `required`.
    pub const fn satisfies(self, required: Fidelity) -> bool {
        match required {
            // Anything that actually ran the code satisfies a JVM-level ask.
            Fidelity::JvmReference => {
                !matches!(self, Fidelity::StaticAnalysis | Fidelity::Simulated)
            }
            Fidelity::NativeRuntime => matches!(
                self,
                Fidelity::NativeRuntime | Fidelity::ArtReference | Fidelity::PhysicalDevice
            ),
            Fidelity::ArtReference => {
                matches!(self, Fidelity::ArtReference | Fidelity::PhysicalDevice)
            }
            Fidelity::PhysicalDevice => matches!(self, Fidelity::PhysicalDevice),
            Fidelity::StaticAnalysis => true,
            Fidelity::Simulated => matches!(self, Fidelity::Simulated),
        }
    }
}

impl std::fmt::Display for Fidelity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Wall/CPU/VM metrics for one request.
///
/// All fields are optional except `wall_ms`; the extra fields make the same
/// object compatible with the existing `run-dex` metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Metrics {
    /// Wall-clock duration in milliseconds.
    #[serde(default)]
    pub wall_ms: u64,
    /// Process CPU time in milliseconds, when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<u64>,
    /// Peak resident set size in bytes, when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<u64>,
    /// VM start/load time in milliseconds (`run-dex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_ms: Option<u64>,
    /// Interpreted VM instructions (`run-dex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<u64>,
    /// Method invokes (`run-dex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invokes: Option<u64>,
    /// Heap allocations (`run-dex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocations: Option<u64>,
    /// Garbage-collection count (`run-dex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gc_count: Option<u64>,
    /// Garbage-collection time in milliseconds (`run-dex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gc_ms: Option<u64>,
    /// Heap peak in bytes (`run-dex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heap_peak_bytes: Option<u64>,
    /// Classes loaded (`run-dex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classes_loaded: Option<u64>,
}

impl Metrics {
    /// Metrics with only a wall time.
    pub const fn wall_ms(ms: u64) -> Self {
        Metrics {
            wall_ms: ms,
            cpu_ms: None,
            peak_rss_bytes: None,
            startup_ms: None,
            instructions: None,
            invokes: None,
            allocations: None,
            gc_count: None,
            gc_ms: None,
            heap_peak_bytes: None,
            classes_loaded: None,
        }
    }

    /// Builder: set CPU time.
    pub const fn with_cpu_ms(mut self, ms: u64) -> Self {
        self.cpu_ms = Some(ms);
        self
    }

    /// Builder: set peak RSS.
    pub const fn with_peak_rss_bytes(mut self, bytes: u64) -> Self {
        self.peak_rss_bytes = Some(bytes);
        self
    }
}

/// Paths to evidence that backs this response.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    /// Fresh screenshot path (relative to the artifacts directory).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    /// UI tree JSON path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    /// Event stream path (JSONL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<String>,
    /// Log file path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// Verdict file path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
}

/// The one JSON object every command returns inside `result`.
///
/// Fields are private so that `ok`, `status` and `error` cannot drift out of
/// sync after construction. Use the getters and builders.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Envelope {
    request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    job_id: Option<String>,
    ok: bool,
    status: Status,
    backend: Backend,
    fidelity: Fidelity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    validated_against: Option<Fidelity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    isolation: Option<IsolationLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frame: Option<FrameMeta>,
    #[serde(default)]
    capabilities_used: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<ProtocolError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(default)]
    metrics: Metrics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<Evidence>,
}

impl Envelope {
    #[allow(clippy::too_many_arguments)]
    fn from_parts(
        request_id: String,
        job_id: Option<String>,
        ok: bool,
        status: Status,
        backend: Backend,
        fidelity: Fidelity,
        validated_against: Option<Fidelity>,
        session_id: Option<String>,
        isolation: Option<IsolationLevel>,
        frame: Option<FrameMeta>,
        capabilities_used: Vec<String>,
        error: Option<ProtocolError>,
        result: Option<Value>,
        metrics: Metrics,
        evidence: Option<Evidence>,
    ) -> Result<Self, EnvelopeError> {
        let envelope = Envelope {
            request_id,
            job_id,
            ok,
            status,
            backend,
            fidelity,
            validated_against,
            session_id,
            isolation,
            frame,
            capabilities_used,
            error,
            result,
            metrics,
            evidence,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    /// A successful response. The only way to get `ok: true`.
    pub fn pass(
        request_id: impl Into<String>,
        backend: Backend,
        fidelity: Fidelity,
        metrics: Metrics,
    ) -> Self {
        Envelope {
            request_id: request_id.into(),
            job_id: None,
            ok: true,
            status: Status::Pass,
            backend,
            fidelity,
            validated_against: None,
            session_id: None,
            isolation: None,
            frame: None,
            capabilities_used: Vec::new(),
            error: None,
            result: None,
            metrics,
            evidence: None,
        }
    }

    /// A failure response. The status is derived from the error code so an
    /// unsupported capability or app crash cannot be mislabelled as `fail`.
    pub fn failure(
        request_id: impl Into<String>,
        backend: Backend,
        fidelity: Fidelity,
        error: ProtocolError,
        metrics: Metrics,
    ) -> Self {
        Envelope {
            request_id: request_id.into(),
            job_id: None,
            ok: false,
            status: error.code.default_status(),
            backend,
            fidelity,
            validated_against: None,
            session_id: None,
            isolation: None,
            frame: None,
            capabilities_used: Vec::new(),
            error: Some(error),
            result: None,
            metrics,
            evidence: None,
        }
    }

    /// Check the structural invariants. Constructors already enforce them; this
    /// is public for deserializers and tests.
    pub fn validate(&self) -> Result<(), EnvelopeError> {
        if self.ok != self.status.is_pass() {
            return Err(EnvelopeError::Invariant(format!(
                "ok={} is inconsistent with status={}",
                self.ok, self.status
            )));
        }
        match (self.status.is_pass(), self.error.as_ref()) {
            (true, Some(err)) => {
                return Err(EnvelopeError::Invariant(format!(
                    "status=pass must not carry error {}",
                    err.code
                )));
            }
            (false, None) => {
                return Err(EnvelopeError::Invariant(format!(
                    "status={} must carry an error object",
                    self.status
                )));
            }
            (false, Some(err)) => {
                let expected = err.code.default_status();
                if expected != self.status {
                    return Err(EnvelopeError::Invariant(format!(
                        "error code {} maps to status {}, not {}",
                        err.code, expected, self.status
                    )));
                }
            }
            (true, None) => {}
        }
        if let Some(err) = &self.error {
            if err.code.is_unsupported() && self.status != Status::Unsupported {
                return Err(EnvelopeError::Invariant(
                    "unsupported error code must use status=unsupported".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Attach a typed result value.
    pub fn with_result<T: Serialize>(mut self, value: T) -> Result<Self, EnvelopeError> {
        self.result = Some(
            serde_json::to_value(value)
                .map_err(|e| EnvelopeError::ResultSerialization(e.to_string()))?,
        );
        Ok(self)
    }

    /// Attach an already-built JSON result.
    pub fn with_value_result(mut self, value: Value) -> Self {
        self.result = Some(value);
        self
    }

    /// Read the result as a typed value.
    pub fn typed_result<T: serde::de::DeserializeOwned>(&self) -> Result<Option<T>, EnvelopeError> {
        match &self.result {
            None => Ok(None),
            Some(value) => serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|e| EnvelopeError::ResultDeserialization(e.to_string())),
        }
    }

    /// Set the job id.
    pub fn with_job_id(mut self, job_id: impl Into<String>) -> Self {
        self.job_id = Some(job_id.into());
        self
    }

    /// Set the session id.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Set the isolation label.
    pub fn with_isolation(mut self, isolation: IsolationLevel) -> Self {
        self.isolation = Some(isolation);
        self
    }

    /// Attach the frame that backs this response.
    pub fn with_frame(mut self, frame: FrameMeta) -> Self {
        self.frame = Some(frame);
        self
    }

    /// Attach the evidence paths.
    pub fn with_evidence(mut self, evidence: Evidence) -> Self {
        self.evidence = Some(evidence);
        self
    }

    /// Replace the used-capability list.
    pub fn with_capabilities<I, S>(mut self, capabilities: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.capabilities_used = capabilities.into_iter().map(Into::into).collect();
        self
    }

    /// Record which fidelity actually validated this result (usually for a
    /// `native-runtime` result checked against an `art-reference` oracle).
    pub fn with_validated_against(mut self, fidelity: Fidelity) -> Self {
        self.validated_against = Some(fidelity);
        self
    }

    /// The stable request id.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// The optional job id.
    pub fn job_id(&self) -> Option<&str> {
        self.job_id.as_deref()
    }

    /// True only for a pass.
    pub fn ok(&self) -> bool {
        self.ok
    }

    /// The outcome status.
    pub fn status(&self) -> Status {
        self.status
    }

    /// The backend that produced the result.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// The achieved fidelity.
    pub fn fidelity(&self) -> Fidelity {
        self.fidelity
    }

    /// The fidelity that validated this result, if any.
    pub fn validated_against(&self) -> Option<Fidelity> {
        self.validated_against
    }

    /// The session id, when the command touched a session.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// The isolation label, when the command touched a session.
    pub fn isolation(&self) -> Option<IsolationLevel> {
        self.isolation
    }

    /// The frame attached to this response, if any.
    pub fn frame(&self) -> Option<&FrameMeta> {
        self.frame.as_ref()
    }

    /// Capabilities used by this request.
    pub fn capabilities_used(&self) -> &[String] {
        &self.capabilities_used
    }

    /// The error, for every non-pass status.
    pub fn error(&self) -> Option<&ProtocolError> {
        self.error.as_ref()
    }

    /// The raw result value.
    pub fn result(&self) -> Option<&Value> {
        self.result.as_ref()
    }

    /// Mutable access to the result, for a server that annotates a **forwarded**
    /// reply with its own bookkeeping (FB5: `label`/`idle_ttl_s` on a launch,
    /// and the owning server's session list on `acos_status`). It never invents
    /// a result on a failure envelope.
    pub fn result_mut(&mut self) -> Option<&mut Value> {
        self.result.as_mut()
    }

    /// The metrics.
    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// The evidence paths.
    pub fn evidence(&self) -> Option<&Evidence> {
        self.evidence.as_ref()
    }
}

/// Why an envelope could not be constructed or deserialized.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    /// The structural pass/error invariants were violated.
    #[error("envelope invariant violated: {0}")]
    Invariant(String),
    /// A typed result could not be serialized.
    #[error("result serialization failed: {0}")]
    ResultSerialization(String),
    /// A typed result could not be deserialized.
    #[error("result deserialization failed: {0}")]
    ResultDeserialization(String),
}

#[derive(Deserialize)]
struct EnvelopeRepr {
    request_id: String,
    #[serde(default)]
    job_id: Option<String>,
    ok: bool,
    status: Status,
    backend: Backend,
    fidelity: Fidelity,
    #[serde(default)]
    validated_against: Option<Fidelity>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    isolation: Option<IsolationLevel>,
    #[serde(default)]
    frame: Option<FrameMeta>,
    #[serde(default)]
    capabilities_used: Vec<String>,
    #[serde(default)]
    error: Option<ProtocolError>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    metrics: Metrics,
    #[serde(default)]
    evidence: Option<Evidence>,
}

impl<'de> Deserialize<'de> for Envelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let repr = EnvelopeRepr::deserialize(deserializer)?;
        Envelope::from_parts(
            repr.request_id,
            repr.job_id,
            repr.ok,
            repr.status,
            repr.backend,
            repr.fidelity,
            repr.validated_against,
            repr.session_id,
            repr.isolation,
            repr.frame,
            repr.capabilities_used,
            repr.error,
            repr.result,
            repr.metrics,
            repr.evidence,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn pass_is_the_only_ok_envelope() {
        let env = Envelope::pass(
            "req_1",
            Backend::HostTooling,
            Fidelity::StaticAnalysis,
            Metrics::wall_ms(3),
        );
        assert!(env.ok());
        assert_eq!(env.status(), Status::Pass);
        assert!(env.validate().is_ok());
    }

    #[test]
    fn failure_status_is_derived_from_code() {
        let env = Envelope::failure(
            "req_2",
            Backend::AcosNative,
            Fidelity::NativeRuntime,
            ProtocolError::new(ErrorCode::UnsupportedAndroidApi, "api 34 widget"),
            Metrics::wall_ms(4),
        );
        assert!(!env.ok());
        assert_eq!(env.status(), Status::Unsupported);
        assert_eq!(env.error().unwrap().code, ErrorCode::UnsupportedAndroidApi);
    }

    #[test]
    fn invalid_json_envelopes_are_rejected_on_deserialize() {
        let false_pass = serde_json::json!({
            "request_id": "req_1",
            "ok": true,
            "status": "fail",
            "backend": "acos-native",
            "fidelity": "native-runtime",
            "capabilities_used": [],
            "error": {"code": "ASSERTION_FAILED", "message": "x", "hint": "fix", "retryable": false},
            "metrics": {"wall_ms": 1}
        });
        assert!(serde_json::from_value::<Envelope>(false_pass).is_err());

        let ok_with_error = serde_json::json!({
            "request_id": "req_1",
            "ok": true,
            "status": "pass",
            "backend": "acos-native",
            "fidelity": "native-runtime",
            "capabilities_used": [],
            "error": {"code": "TIMEOUT", "message": "x", "hint": "fix", "retryable": true},
            "metrics": {"wall_ms": 1}
        });
        assert!(serde_json::from_value::<Envelope>(ok_with_error).is_err());

        let unsupported_as_error = serde_json::json!({
            "request_id": "req_1",
            "ok": false,
            "status": "error",
            "backend": "acos-native",
            "fidelity": "native-runtime",
            "capabilities_used": [],
            "error": {"code": "UNSUPPORTED_JAVA_API", "message": "x", "hint": "fix", "retryable": false},
            "metrics": {"wall_ms": 1}
        });
        assert!(serde_json::from_value::<Envelope>(unsupported_as_error).is_err());
    }

    #[test]
    fn typed_results_round_trip() {
        let env = Envelope::pass(
            "req_1",
            Backend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(1),
        )
        .with_result(serde_json::json!({"hello": "world"}))
        .unwrap()
        .with_session_id("s_1")
        .with_isolation(IsolationLevel::Sandbox);
        let json = serde_json::to_string(&env).unwrap();
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, env);
        let typed: Option<Value> = back.typed_result().unwrap();
        assert_eq!(typed.unwrap()["hello"], "world");
    }
}
