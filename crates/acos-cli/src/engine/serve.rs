//! Real-ART sessions behind `acos serve` (W50).
//!
//! `acos serve` is the daemon/MCP transport agents actually use. Before W50 it
//! held only [`acos_mcp::SessionBackend`] sessions (the frozen Rust VM
//! `AgentSession`), so every launch through the socket died in the Rust VM and
//! no agent could reach a verdict. This module puts the **same real-ART
//! machinery** `acos verify --engine art` already uses
//! ([`super::art::ArtSession`], one `acos-art launch --interactive` guest per
//! session, the line-JSON tree/tap/frames protocol) behind the socket:
//!
//! * the default engine is `art`; the Rust VM stays reachable only through an
//!   explicit `engine: "rust-vm"`, and every reply from it is labelled
//!   `engine: "rust-vm, frozen"`;
//! * an unknown engine is a structured `UNSUPPORTED_BACKEND`, never a silent
//!   fallback to another engine;
//! * a verb ART cannot do (`acos_swipe`) returns a structured
//!   `UNSUPPORTED_BACKEND` naming the missing capability, never a fake success;
//! * every reply carries `engine`, `trust` and the evidence label
//!   (`host-pixel` when the reply hands over host-observed pixels,
//!   `guest-asserted` for the guest's own tree/logs/input acks), matching the
//!   provenance labels `verify` records (`crates/acos-cli/src/trust.rs`);
//! * `acos_reset` is the engine's honest reset: the guest process is stopped and
//!   the same APK is launched again in a fresh process with empty app-data, so
//!   the frame after a reset is provably newer than the one before it;
//! * `acos_destroy` closes the guest and reports the teardown check
//!   (process group empty) — a survivor is a failure, never a quiet success.
//!
//! One ART guest per serve session id. The guest is owned by an
//! `Arc<Mutex<ArtSession>>`, so two connections addressing the same session
//! serialise instead of racing two input streams into one app.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use acos_mcp::{CallContext, SessionBackend as LegacyBackend};
use acos_protocol::ui::{Bounds, UiNode, UiTree};
use acos_protocol::{
    Backend as ProtocolBackend, Envelope, ErrorCode, Fidelity, LogLevel, Metrics, ProtocolError,
    Selector,
};
use acos_verify::BackendError;
use serde_json::{json, Value};

use crate::trust::Trust;

use super::art::{m5_now_us, m5_trace, p3_enabled, p3_trace, ArtSession, GuestExit};
use super::{Engine, SessionBackend};

/// The engine label every ART reply carries.
pub const ENGINE_ART: &str = "art";
/// The engine label every Rust VM reply carries. It is frozen by policy.
pub const ENGINE_RUST_VM: &str = "rust-vm, frozen";
/// The engine label for the confined native child (Flutter/RN/native), which is
/// real host native code, not the frozen Rust VM.
pub const ENGINE_NATIVE_CHILD: &str = "native-child";

/// The evidence label for pixels the host captured from the guest surface.
pub const EVIDENCE_HOST_PIXEL: &str = "host-pixel";
/// The evidence label for anything only the guest asserts (tree, logs, acks).
pub const EVIDENCE_GUEST_ASSERTED: &str = "guest-asserted";

/// A socket session held by this daemon.
struct HeldArtSession {
    apk: PathBuf,
    /// Where this guest's frames and its `session-exit.json` go. Needed to
    /// classify a lost guest (W55).
    artifacts: PathBuf,
    trust: Trust,
    /// `Mutex` because two connections may address the same session id; the
    /// guest takes one command at a time.
    session: Mutex<ArtSession>,
}

/// Session routing for the daemon: which engine owns a session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Art,
    Legacy,
}

/// The daemon's engine multiplexer.
pub struct ServeEngine {
    legacy: Arc<LegacyBackend>,
    default_engine: Engine,
    max_sessions: usize,
    artifacts_root: PathBuf,
    /// VIEW1a: the live-view registry this daemon publishes its sessions to
    /// (`$XDG_RUNTIME_DIR/acos/live`, protocol v1 §5). It writes nothing until a
    /// session reaches ready.
    live: crate::live_registry::LiveRegistry,
    art: Mutex<HashMap<String, Arc<HeldArtSession>>>,
    routes: Mutex<HashMap<String, Route>>,
}

impl ServeEngine {
    /// Build the multiplexer.
    ///
    /// `artifacts_root` is where ART frame mirrors go; the default is
    /// `$ACOS_RUNTIME_DIR/art-sessions`, inside the daemon's private runtime
    /// directory (never `/tmp`, never the repo root).
    pub fn new(
        legacy: Arc<LegacyBackend>,
        default_engine: Engine,
        max_sessions: usize,
        artifacts_root: PathBuf,
    ) -> Self {
        ServeEngine {
            legacy,
            default_engine,
            max_sessions: max_sessions.max(1),
            artifacts_root,
            live: crate::live_registry::LiveRegistry::from_env(),
            art: Mutex::new(HashMap::new()),
            routes: Mutex::new(HashMap::new()),
        }
    }

    /// Number of sessions held (ART + legacy). Used by the idle-exit check and
    /// the `--max-sessions` cap; it never probes a guest.
    pub fn session_count(&self) -> usize {
        let art = self.art.lock().unwrap_or_else(|e| e.into_inner()).len();
        art + self.legacy.session_count()
    }

    /// Close every held ART guest (daemon shutdown). Returns how many were
    /// closed. The legacy backend's sessions die with the process as before.
    pub fn shutdown(&self) -> usize {
        let held: Vec<(String, Arc<HeldArtSession>)> = {
            let mut map = self.art.lock().unwrap_or_else(|e| e.into_inner());
            map.drain().collect()
        };
        let count = held.len();
        for (id, held) in held {
            let mut session = held.session.lock().unwrap_or_else(|e| e.into_inner());
            let _ = session.close();
            eprintln!("acos serve: closed ART session {id}");
        }
        self.routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // VIEW1a: the daemon is exiting; no entry it published may survive it.
        self.live.remove_all();
        count
    }

    /// VIEW1a: publish one ready session to the live-view registry.
    fn publish_live(&self, id: &str, session: &ArtSession) {
        let (client_name, client_version) = self.live.client();
        let entry = crate::live_registry::LiveEntry {
            session_id: id.to_string(),
            daemon_pid: std::process::id(),
            launcher_pid: session.launcher_pid(),
            started_ms: crate::live_registry::now_ms(),
            client_name: client_name.to_string(),
            client_version: client_version.to_string(),
            project_dir: self.live.project_dir().to_string(),
            apk: session.apk_path().display().to_string(),
            package: session.package().to_string(),
            activity: session.activity().map(str::to_string),
            display: session.display_size(),
            stream_socket: session
                .stream_socket()
                .map(|path| path.display().to_string()),
        };
        if let Err(error) = self.live.write(&entry) {
            eprintln!(
                "acos serve: live view: cannot publish session {id} to {}: {error}",
                self.live.dir().display()
            );
        }
    }

    /// VIEW1a: refresh a session's `activity` in the registry.
    fn refresh_live_activity(&self, id: &str, session: &ArtSession) {
        if let Err(error) = self.live.update_activity(id, session.activity()) {
            eprintln!("acos serve: live view: cannot update session {id} activity: {error}");
        }
    }

    /// The daemon's session list, ART sessions and legacy sessions merged.
    pub fn list_sessions(&self) -> Value {
        let mut merged = self.legacy.list_sessions();
        let mut sessions = merged
            .get("sessions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let art = self.art.lock().unwrap_or_else(|e| e.into_inner());
        for (id, held) in art.iter() {
            let mut session = held.session.lock().unwrap_or_else(|e| e.into_inner());
            // A dead guest is reported as dead, with its own exit facts, never
            // as a live frame sequence (W55).
            let exit = session.liveness();
            sessions.push(json!({
                "session_id": id,
                "launched": true,
                "package": session.package(),
                "apk": held.apk.display().to_string(),
                "stage_reached": "first_frame",
                "frame_seq": if exit.is_some() {
                    0
                } else {
                    session.frame_info().map(|f| f.0).unwrap_or(0)
                },
                "engine": ENGINE_ART,
                "boot_image": session.boot_image().cloned().unwrap_or(Value::Null),
                "mode": if exit.is_some() { "dead" } else { "live" },
                "live": exit.is_none(),
                "guest_exit": exit.as_ref().map(GuestExit::to_json),
                "native": true,
                "trust": held.trust.as_str(),
            }));
        }
        let count = sessions.len();
        merged["sessions"] = Value::Array(sessions);
        merged["count"] = json!(count);
        merged["engine"] = json!(self.default_engine.as_str());
        merged
    }

    fn route_of(&self, id: &str) -> Option<Route> {
        self.routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .copied()
    }

    fn set_route(&self, id: &str, route: Route) {
        self.routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), route);
    }

    fn held(&self, id: &str) -> Option<Arc<HeldArtSession>> {
        self.art
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// Serve one session-tool request.
    pub fn call(&self, tool: &str, args: &Value, ctx: &CallContext) -> Envelope {
        let request_id = request_id(args);
        let id = session_id(args);

        if tool == "ping" {
            return Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_value_result(json!({
                "pong": true,
                "pid": std::process::id(),
                "engine": self.default_engine.as_str(),
                "default_engine": self.default_engine.as_str(),
                "trust": Trust::Trusted.as_str(),
                "evidence": EVIDENCE_NO_GUEST,
            }));
        }
        if tool == "session_list" {
            return Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_value_result(self.list_sessions());
        }

        // Engine selection: explicit wins; a held ART session wins over an
        // unspoken default (an agent that launched ART keeps talking to it).
        let explicit = match parse_engine_arg(args) {
            Ok(value) => value,
            Err(message) => {
                return unsupported_engine(&request_id, &message, self.default_engine);
            }
        };
        let held = self.held(&id);
        let art_target = match explicit {
            Some(Engine::RustVm) => None,
            Some(Engine::Art) => held.clone(),
            None => match self.route_of(&id) {
                Some(Route::Legacy) => None,
                Some(Route::Art) | None => held.clone(),
            },
        };
        if let (Some(Engine::RustVm), Some(_)) = (explicit, held.as_ref()) {
            if tool != "acos_destroy" {
                return art_failure(
                    &request_id,
                    Some(&id),
                    Trust::Trusted,
                    ErrorCode::SessionBusy,
                    format!(
                        "session `{id}` is held by the ART engine; `engine: rust-vm` cannot \
                         address it"
                    ),
                    json!({"held_engine": ENGINE_ART, "requested_engine": ENGINE_RUST_VM}),
                );
            }
        }

        match tool {
            "acos_launch" => self.call_launch(&request_id, &id, args, ctx, explicit),
            "acos_destroy" => self.call_destroy(&request_id, &id, args, ctx),
            _ => {
                if let Some(held) = art_target {
                    return self.call_art(&request_id, &id, &held, tool, args);
                }
                // Decide which engine owns this id without guessing: an
                // explicit engine wins, otherwise only a session this daemon
                // routed to the legacy backend may use it. Never silently fall
                // back to the Rust VM for an ART (or unknown) session.
                let wants_legacy = match explicit {
                    Some(Engine::RustVm) => true,
                    Some(Engine::Art) => false,
                    None => self.route_of(&id) == Some(Route::Legacy),
                };
                if !wants_legacy {
                    return with_hint(
                        art_failure(
                            &request_id,
                            Some(&id),
                            Trust::Trusted,
                            ErrorCode::SessionNotFound,
                            format!("no live ART session `{id}`; {tool} needs a launched session"),
                            json!({
                                "engine": explicit.unwrap_or(self.default_engine).as_str(),
                                "tool": tool,
                            }),
                        ),
                        "Call acos_launch with this base.session_id first (the default engine \
                         is art). Use `engine: \"rust-vm\"` only if you explicitly want the \
                         frozen Rust VM.",
                    );
                }
                self.call_legacy(&request_id, &id, tool, args, ctx)
            }
        }
    }

    // -- launch ------------------------------------------------------------ //

    fn call_launch(
        &self,
        request_id: &str,
        id: &str,
        args: &Value,
        ctx: &CallContext,
        explicit: Option<Engine>,
    ) -> Envelope {
        let Some(apk_arg) = args.get("apk").and_then(Value::as_str) else {
            return art_failure(
                request_id,
                Some(id),
                Trust::Trusted,
                ErrorCode::InvalidArgument,
                "acos_launch needs a string `apk`",
                json!({"field": "apk"}),
            );
        };
        let native_requested = ["native", "flutter", "rn"]
            .iter()
            .any(|key| args.get(*key).and_then(Value::as_bool).unwrap_or(false));
        // The Rust loader (`native: true`) is a different execution path from
        // ART and cannot be combined with it. The `rn`/`flutter` flags are
        // *product-path hints* for the legacy native-child route, not a claim
        // that ART cannot run the APK: ART boots the app's own launcher Activity
        // and loads its `lib/<abi>/` closure, so an explicit `engine: "art"`
        // wins and those hints are ignored rather than refused (G1 rank 5: a
        // Gaten `--engine art` launch used to die in the frozen VM).
        let rust_loader = args
            .get("native")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if explicit == Some(Engine::Art) && rust_loader {
            return with_hint(
                art_failure(
                    request_id,
                    Some(id),
                    Trust::Trusted,
                    ErrorCode::UnsupportedBackend,
                    "`native: true` (the Rust loader) cannot be combined with engine: \"art\"",
                    json!({
                        "capability": "engine.art.native-loader",
                        "requested_engine": ENGINE_ART,
                        "fallback_used": false,
                    }),
                ),
                "ART has its own process and loads the APK's libraries itself: drop `native` and \
                 launch with engine: \"art\", or ask for engine: \"rust-vm\" to use the Rust loader.",
            );
        }
        let engine = explicit.unwrap_or(if native_requested {
            // Flutter/RN/native sessions on the *default* route are real host
            // native code in a confined child, not the Rust VM; they keep the
            // legacy route and are labelled `native-child`.
            Engine::RustVm
        } else {
            self.default_engine
        });
        if engine == Engine::RustVm {
            // Explicit Rust VM request (or a native-child APK): the legacy
            // route is frozen, and every reply is labelled with the engine that
            // actually ran.
            return self.call_legacy(request_id, id, "acos_launch", args, ctx);
        }
        self.launch_art(request_id, id, apk_arg, args, ctx)
    }

    fn launch_art(
        &self,
        request_id: &str,
        id: &str,
        apk_arg: &str,
        args: &Value,
        ctx: &CallContext,
    ) -> Envelope {
        let trust = trust_arg(args);
        let verb_started = std::time::Instant::now();
        // P9: launch consumes P1/P3b's settle vocabulary. `strict` is the
        // default and unchanged; `fast` binds the first traversal's probe sample
        // instead of waiting out the 150 ms quiet window. The reply says which
        // mode produced it, and a fast reply is never assertion evidence.
        let (settle, pixels) = match launch_bind(args) {
            Ok(bind) => bind,
            Err(error) => return art_error(request_id, Some(id), trust, &error),
        };
        // P12: `animations:"on"|"off"` is a launch-time choice (default on).
        // Every reply for the session states it.
        let animations: &'static str = match args.get("animations").and_then(Value::as_str) {
            None => "on",
            Some(value) => match value.trim().to_ascii_lowercase().as_str() {
                "on" => "on",
                "off" => "off",
                other => {
                    return art_failure(
                        request_id,
                        Some(id),
                        trust,
                        ErrorCode::InvalidArgument,
                        format!("animations must be \"on\" or \"off\" (got {other:?})"),
                        json!({"field": "animations"}),
                    )
                }
            },
        };
        let settle_timeout_ms = settle_timeout_arg(args);
        // W7: app-window inset model, launch-time like `animations`. Defaults
        // to the process's `ACOS_WM_INSETS` (and then the compile-time default)
        // so `acos verify` and a direct `acos-art launch` agree.
        let insets: &'static str = match args.get("insets").and_then(Value::as_str) {
            None => match crate::engine::art::insets_mode_from_env() {
                "report" => "report",
                _ => "carve",
            },
            Some(value) => match acos_services::wms::parse_insets_mode(value) {
                Some(acos_services::wms::InsetsMode::Carve) => "carve",
                Some(acos_services::wms::InsetsMode::Report) => "report",
                None => {
                    return art_failure(
                        request_id,
                        Some(id),
                        trust,
                        ErrorCode::InvalidArgument,
                        format!("insets must be \"carve\" or \"report\" (got {value:?})"),
                        json!({"field": "insets"}),
                    )
                }
            },
        };
        let apk = match acos_mcp::safety::validate_path(apk_arg, &ctx.allowed_roots) {
            Ok(path) => path,
            Err(reason) => {
                return art_failure(
                    request_id,
                    Some(id),
                    trust,
                    ErrorCode::InvalidArgument,
                    format!("refusing APK path `{apk_arg}`: {reason}"),
                    json!({"apk": apk_arg}),
                );
            }
        };
        // A previous guest for this id that has died is never served. Its frame
        // is discarded and a fresh guest replaces it below; the reply says so.
        let mut replaced_guest: Option<Value> = None;
        if let Some(held) = self.held(id) {
            if held.apk != apk {
                return with_hint(
                    art_failure(
                        request_id,
                        Some(id),
                        held.trust,
                        ErrorCode::InvalidArgument,
                        format!(
                            "session `{id}` already runs {}; it cannot switch to {}",
                            held.apk.display(),
                            apk.display()
                        ),
                        json!({
                            "held_apk": held.apk.display().to_string(),
                            "requested_apk": apk.display().to_string(),
                        }),
                    ),
                    "One serve session is one ART guest. Call acos_destroy for this session id, \
                     then acos_launch the other APK.",
                );
            }
            let dead = {
                let mut session = held.session.lock().unwrap_or_else(|e| e.into_inner());
                // P12: animations are launch-time. A relaunch of a live session
                // cannot silently change them.
                if args.get("animations").is_some() && session.animations() != animations {
                    return with_hint(
                        art_failure(
                            request_id,
                            Some(id),
                            held.trust,
                            ErrorCode::InvalidArgument,
                            format!(
                                "session `{id}` runs with animations:{} and a live session cannot \
                                 change it",
                                session.animations()
                            ),
                            json!({
                                "held_animations": session.animations(),
                                "requested_animations": animations,
                            }),
                        ),
                        "animations is launch-time: call acos_destroy for this session id, then \
                         acos_launch with the new choice.",
                    );
                }
                if args.get("insets").is_some() && session.insets() != insets {
                    return with_hint(
                        art_failure(
                            request_id,
                            Some(id),
                            held.trust,
                            ErrorCode::InvalidArgument,
                            format!(
                                "session `{id}` runs with insets:{} and a live session cannot \
                                 change it",
                                session.insets()
                            ),
                            json!({
                                "held_insets": session.insets(),
                                "requested_insets": insets,
                            }),
                        ),
                        "insets is launch-time: call acos_destroy for this session id, then \
                         acos_launch with the new choice.",
                    );
                }
                session.set_settle_timeout(settle_timeout_ms);
                session.liveness().map(|exit| {
                    let discarded = session.invalidate_frame();
                    let classified = crate::failure::classify_session_exit(&held.artifacts)
                        .or_else(|| {
                            exit.exit_code.or(exit.signal).and_then(|_| {
                                crate::failure::classify_child_exit(exit_status(&exit))
                            })
                        });
                    json!({
                        "guest_exit": exit.to_json(),
                        "session_exit": classified.map(|(code, facts)| json!({
                            "code": code.as_str(),
                            "facts": facts,
                        })),
                        "discarded_frame": discarded.map(|(seq, md5, at)| json!({
                            "frame_seq": seq,
                            "md5": md5,
                            "captured_at": at,
                            "served": false,
                        })),
                    })
                })
            };
            match dead {
                None => {
                    // Re-launch on the live session = hand back its bound live
                    // frame (the daemon's whole point); never boot a second
                    // guest.
                    let mut session = held.session.lock().unwrap_or_else(|e| e.into_inner());
                    return match session.ensure_launch_tree(settle, pixels) {
                        Ok(tree) => launch_reply(
                            request_id,
                            id,
                            &held,
                            &session,
                            &tree,
                            false,
                            None,
                            verb_started,
                        ),
                        Err(error) => art_error(request_id, Some(id), held.trust, &error),
                    };
                }
                Some(details) => {
                    // Dead: drop the held session (dropping closes and reaps the
                    // process group) and start a fresh guest for the same APK.
                    self.art
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(id);
                    let mut session = held.session.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = session.teardown();
                    replaced_guest = Some(details);
                }
            }
        }
        if self.session_count() >= self.max_sessions {
            return art_failure(
                request_id,
                Some(id),
                trust,
                ErrorCode::ResourceLimit,
                format!(
                    "session cap reached ({}); destroy a session first",
                    self.max_sessions
                ),
                json!({"max_sessions": self.max_sessions}),
            );
        }
        let artifacts = self.artifacts_root.join(sanitize_session_id(id));
        match ArtSession::start_with_options(&apk, &artifacts, animations, insets) {
            Ok(mut session) => {
                session.set_settle_timeout(settle_timeout_ms);
                let started = match session.ensure_launch_tree(settle, pixels) {
                    Ok(tree) => tree,
                    Err(error) => {
                        // G3f1: a guest that dies before its first frame must
                        // keep its evidence and be classified. The launcher
                        // writes `session-exit.json` (exit status, logd tail,
                        // native-crash record) as it tears down, i.e. after the
                        // reply the engine already classified; closing first and
                        // re-reading is what makes the record complete.
                        let evidence = session.keep_launch_failure_evidence(&error);
                        return art_error_with_launch_evidence(
                            request_id,
                            Some(id),
                            trust,
                            &error,
                            evidence,
                        );
                    }
                };
                let held = Arc::new(HeldArtSession {
                    apk: apk.clone(),
                    artifacts: artifacts.clone(),
                    trust,
                    session: Mutex::new(session),
                });
                self.art
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id.to_string(), Arc::clone(&held));
                self.set_route(id, Route::Art);
                // VIEW1a: publish the session so a viewer hub can find its
                // stream socket. A registry write failure is logged and never
                // fails the launch (the session itself is fine).
                {
                    let session = held.session.lock().unwrap_or_else(|e| e.into_inner());
                    self.publish_live(id, &session);
                }
                let guard = held.session.lock().unwrap_or_else(|e| e.into_inner());
                launch_reply(
                    request_id,
                    id,
                    &held,
                    &guard,
                    &started,
                    true,
                    replaced_guest,
                    verb_started,
                )
            }
            Err(error) => {
                // Structured, and never a silent fallback to the Rust VM.
                art_error(request_id, Some(id), trust, &error)
            }
        }
    }

    // -- destroy ----------------------------------------------------------- //

    fn call_destroy(
        &self,
        request_id: &str,
        id: &str,
        args: &Value,
        ctx: &CallContext,
    ) -> Envelope {
        let held = self
            .art
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(held) = held {
            let trust = held.trust;
            self.routes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(id);
            // VIEW1a: the session is gone; its registry entry must not outlive
            // it (a hub must never dial a socket that no longer exists).
            self.live.remove(id);
            let mut session = held.session.lock().unwrap_or_else(|e| e.into_inner());
            let report = session.teardown();
            // GL1: name the destruction path and prove the *session*, not just
            // the spawned process group, is gone. `lifecycle` = the guest ended
            // by itself after the close (onPause/onStop/onDestroy delivered and
            // returned); `killed` = the bounded grace expired and the tree was
            // killed, which is what Android's AMS does to a process whose
            // threads never exit. Neither path waits for app threads.
            let (path, teardown_ms) = session
                .last_teardown()
                .map(|(path, ms)| (path.as_str(), ms))
                .unwrap_or(("unknown", 0));
            let leftovers = session.session_leftovers();
            let (ok, detail) = match report {
                Ok(report) => {
                    let clean = report.is_clean() && leftovers.is_empty();
                    (
                        clean,
                        json!({
                            "process_group": report.process_group,
                            "survivors": report.survivors,
                            "verified": report.verified,
                            "lifecycle": path,
                            "teardown_ms": teardown_ms,
                            "session_leftovers": leftovers,
                        }),
                    )
                }
                Err(error) => (false, json!({"error": error.to_protocol_error()})),
            };
            let mut result = json!({
                "session_id": id,
                "destroyed": true,
                "already_absent": false,
                "teardown": detail,
                "engine": ENGINE_ART,
            });
            label_meta(&mut result, ENGINE_ART, trust, EVIDENCE_NO_GUEST);
            if !ok {
                return with_hint(
                    art_failure(
                        request_id,
                        Some(id),
                        trust,
                        ErrorCode::Internal,
                        format!("PROCESS_TREE_DIRTY: session `{id}` teardown is not clean"),
                        json!({"teardown": result["teardown"].clone()}),
                    ),
                    "The guest process group was signalled but processes survive. Inspect /proc \
                     and systemd-cgls for the session group before starting another session.",
                );
            }
            return Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::ArtReference,
                Metrics::wall_ms(0),
            )
            .with_session_id(id.to_string())
            .with_value_result(result);
        }
        let existed = self
            .routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
            .is_some();
        let env = self.legacy.session_call("acos_destroy", args, ctx);
        let env = match env {
            Some(env) => relabel_legacy(env, ENGINE_RUST_VM, Trust::Trusted, true),
            None => art_failure(
                request_id,
                Some(id),
                Trust::Trusted,
                ErrorCode::UnsupportedBackend,
                "the legacy backend has no acos_destroy",
                json!({"tool": "acos_destroy"}),
            ),
        };
        if !existed {
            // A destroy of an unknown id is an honest no-op, as before.
            if let Some(result) = env.result().cloned() {
                let mut result = result;
                if let Some(object) = result.as_object_mut() {
                    object.insert("already_absent".into(), json!(true));
                }
                return with_result(env, result);
            }
        }
        env
    }

    // -- ART verbs --------------------------------------------------------- //

    fn call_art(
        &self,
        request_id: &str,
        id: &str,
        held: &Arc<HeldArtSession>,
        tool: &str,
        args: &Value,
    ) -> Envelope {
        let trust = held.trust;
        let mut session = held.session.lock().unwrap_or_else(|e| e.into_inner());
        // Correctness first: a dead guest must never answer from its cache.
        // Every verb starts with the child's own liveness, not with a frame.
        if let Some(exit) = session.liveness() {
            return guest_exit_envelope(request_id, id, held, &mut session, tool, &exit);
        }
        // Then policy: the same fail-closed rules verify's TrustGuard applies.
        if let Some((assertion, message)) = trust_refusal(trust, tool, args) {
            return art_failure(
                request_id,
                Some(id),
                trust,
                ErrorCode::InsufficientHostEvidence,
                message,
                json!({
                    "engine": ENGINE_ART,
                    "verb": tool,
                    "assertion": assertion,
                    "trust_guard": "crate::trust::TrustGuard (observe/logs_since fail closed)",
                    "allowed": ["acos_observe (frame only)", "acos_tap", "acos_key", "acos_text"],
                }),
            );
        }
        // A verb that mutates the guest but was refused above never reaches it;
        // a verb whose *own* preconditions need the tree (tap by selector) is
        // still allowed: navigation only, never a pass.
        let started = std::time::Instant::now();
        let p3_t_call = m5_now_us();
        // P12b: one per-request settle deadline for whichever verb runs next;
        // the guest still answers a timeout with the latest tree/frame identity.
        session.set_settle_timeout(settle_timeout_arg(args));
        let outcome = match tool {
            "acos_observe_tree" => observe_tree(&mut session, args),
            "acos_observe" => observe(&mut session, args),
            "acos_tap" => tap(&mut session, args),
            "acos_text" => type_text(&mut session, args),
            "acos_key" => key(&mut session, args),
            "acos_assert" => assert(&mut session, args, trust),
            "acos_logs" => logs(&mut session),
            "acos_status" => Ok(status_value(&session)),
            "acos_reset" => reset(&mut session, args),
            "acos_swipe" => Err(BackendError::unsupported(
                "input.swipe",
                "the ART session engine has no swipe command (tap/key/type_text only)",
            )),
            "acos_action" => action(&mut session, args),
            other => {
                return art_failure(
                    request_id,
                    Some(id),
                    trust,
                    ErrorCode::UnsupportedBackend,
                    format!("unknown session tool `{other}`"),
                    json!({"tool": other, "engine": ENGINE_ART}),
                )
            }
        };
        match outcome {
            Ok(result) => {
                let p3_t_outcome = m5_now_us();
                let mut result = result;
                result["session_id"] = json!(id);
                if matches!(tool, "acos_tap" | "acos_text" | "acos_key" | "acos_action") {
                    if let Err(error) = attach_observation(&mut session, &mut result, args) {
                        return art_error(request_id, Some(id), trust, &error);
                    }
                }
                label_meta(&mut result, ENGINE_ART, trust, evidence_for(tool));
                if let Some(object) = result.as_object_mut() {
                    object.insert("animations".into(), json!(session.animations()));
                    object.insert("insets".into(), json!(session.insets()));
                }
                label_frame(&mut result, &session);
                label_timing(&mut result, &session, tool, started);
                // VIEW1a: keep the registry's `activity` in step with the
                // session's own state (a no-op when it has not changed).
                self.refresh_live_activity(id, &session);
                let envelope = Envelope::pass(
                    request_id,
                    ProtocolBackend::AcosNative,
                    Fidelity::ArtReference,
                    Metrics::wall_ms(0),
                )
                .with_session_id(id.to_string())
                .with_value_result(result);
                let envelope = envelope.with_capabilities(capabilities_for(tool));
                if p3_enabled() {
                    p3_trace(
                        "serve",
                        "serve.call_art",
                        p3_t_call,
                        m5_now_us(),
                        &format!(
                            "tool={tool} outcome_us={}",
                            p3_t_outcome.saturating_sub(p3_t_call)
                        ),
                    );
                }
                envelope
            }
            Err(error) => art_error(request_id, Some(id), trust, &error),
        }
    }

    // -- legacy (frozen Rust VM / confined native child) -------------------- //

    fn call_legacy(
        &self,
        request_id: &str,
        id: &str,
        tool: &str,
        args: &Value,
        ctx: &CallContext,
    ) -> Envelope {
        let native_requested = ["native", "flutter", "rn"]
            .iter()
            .any(|key| args.get(*key).and_then(Value::as_bool).unwrap_or(false));
        let label = if native_requested {
            ENGINE_NATIVE_CHILD
        } else {
            ENGINE_RUST_VM
        };
        let frozen = label == ENGINE_RUST_VM;
        let trust = trust_arg(args);
        let Some(envelope) = self.legacy.session_call(tool, args, ctx) else {
            return art_failure(
                request_id,
                Some(id),
                trust,
                ErrorCode::UnsupportedBackend,
                format!("unknown session tool `{tool}`"),
                json!({"tool": tool, "engine": self.default_engine.as_str()}),
            );
        };
        if tool == "acos_launch" {
            self.set_route(id, Route::Legacy);
        } else if tool == "acos_destroy" {
            self.routes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(id);
        }
        relabel_legacy(envelope, label, trust, frozen)
    }
}

/// The trust rules serve applies to an untrusted session, mirroring
/// [`crate::trust::TrustGuard`] on the verify path.
///
/// `TrustGuard` refuses exactly two channels on an untrusted run: `observe`
/// (the UI tree) and `logs_since` (guest log lines), with
/// `INSUFFICIENT_HOST_EVIDENCE`. It still allows `capture` (host pixels),
/// `input` (host-injected; a selector only navigates) and session lifecycle.
/// Serve maps the same channels:
///
/// * `acos_observe_tree`, `acos_assert`, `acos_observe {tree|text}` -> `observe`
/// * `acos_logs`                                                  -> `logs_since`
/// * `acos_observe` with neither `tree` nor `text` (frame/screenshot) -> `capture`
/// * `acos_tap` / `acos_key` / `acos_text` / `acos_action`          -> `input`
///
/// A refusal names the assertion class in the message so an agent can tell a
/// policy refusal from a guest failure.
fn trust_refusal(trust: Trust, tool: &str, args: &Value) -> Option<(&'static str, String)> {
    if trust != Trust::Untrusted {
        return None;
    }
    let wants_tree = args.get("tree").and_then(Value::as_bool).unwrap_or(false)
        || args.get("text").and_then(Value::as_bool).unwrap_or(false);
    match tool {
        "acos_observe_tree" | "acos_assert" => Some((
            "UNTRUSTED_TREE_ASSERTION",
            format!(
                "{tool} on an untrusted run: the UI tree is guest-asserted and cannot justify a \
                 pass. Use a host-observed assertion (acos_observe frame / screenshot region) \
                 instead; the tree still resolves tap targets for navigation."
            ),
        )),
        "acos_observe" if wants_tree => Some((
            "UNTRUSTED_TREE_ASSERTION",
            "acos_observe with `tree`/`text` on an untrusted run: the tree is guest-asserted \
             and cannot justify a pass. Ask for the frame only (tree:false, text:false) or use \
             a host-observed region assertion."
                .to_string(),
        )),
        "acos_logs" => Some((
            "UNTRUSTED_LOG_ASSERTION",
            "acos_logs on an untrusted run: the guest log is guest-asserted and cannot justify \
             a pass. Use a host-observed assertion (acos_observe frame / screenshot region)."
                .to_string(),
        )),
        _ => None,
    }
}

/// One ART verb's reply when the guest behind the session is not alive.
///
/// A dead session has no frame to serve. The cached frame is dropped first
/// (its identity is reported, never handed over), and the code comes from the
/// session's own `session-exit.json` when it wrote one, else from the child's
/// exit status (`crate::failure::classify_child_exit`), else `GUEST_EXITED`.
/// `INTERNAL` is never used for a guest death (W53/W55).
fn guest_exit_envelope(
    request_id: &str,
    id: &str,
    held: &HeldArtSession,
    session: &mut ArtSession,
    verb: &str,
    exit: &GuestExit,
) -> Envelope {
    let discarded = session.invalidate_frame();
    let classified = crate::failure::classify_session_exit(&held.artifacts).or_else(|| {
        exit.exit_code
            .or(exit.signal)
            .and_then(|_| crate::failure::classify_child_exit(exit_status(exit)))
    });
    let (code, facts) = match classified {
        Some((code, facts)) => (code, json!(facts)),
        None => (ErrorCode::GuestExited, Value::Null),
    };
    let detail = format!(
        "GUEST_EXITED: the ART guest for session `{id}` is not alive ({}), so `{verb}` has no \
         live frame to observe; the cached frame was discarded, not served",
        describe_exit(exit)
    );
    let details = json!({
        "verb": verb,
        "guest_exit": exit.to_json(),
        "session_exit": facts,
        "session_exit_path": crate::failure::session_exit_path(&held.artifacts)
            .display()
            .to_string(),
        "discarded_frame": discarded.map(|(seq, md5, at)| json!({
            "frame_seq": seq,
            "md5": md5,
            "captured_at": at,
            "served": false,
        })),
        "launch_hint": "call acos_launch with this session id to start a fresh guest",
    });
    with_hint(
        art_failure(request_id, Some(id), held.trust, code, detail, details),
        "The guest process ended. `acos_launch` on this session id relaunches a fresh guest \
         (it never returns the dead session's frame). Read details.session_exit / \
         details.guest_exit and the session's logcat before retrying the same app.",
    )
}

/// Rebuild the exit status the child reported (`classify_child_exit` takes a
/// real `ExitStatus`). `GuestExit` keeps the raw code/signal for exactly this.
fn exit_status(exit: &GuestExit) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    match (exit.signal, exit.exit_code) {
        // Signal-only: the wait status stores the signal in the low 7 bits.
        (Some(signal), _) => std::process::ExitStatus::from_raw(signal),
        (None, Some(code)) => std::process::ExitStatus::from_raw(code << 8),
        (None, None) => std::process::ExitStatus::from_raw(0),
    }
}

fn describe_exit(exit: &GuestExit) -> String {
    match (exit.signal, exit.exit_code) {
        (Some(signal), _) => format!("{}: signal {signal}", exit.state),
        (None, Some(code)) => format!("{}: exit {code}", exit.state),
        (None, None) => exit.state.clone(),
    }
}

/// The labels of a reply that carries no guest observation (status/destroy).
const EVIDENCE_NO_GUEST: &str = "none";

fn evidence_for(tool: &str) -> &'static str {
    match tool {
        "acos_observe" | "acos_screenshot" => EVIDENCE_HOST_PIXEL,
        "acos_observe_tree" | "acos_assert" => EVIDENCE_GUEST_ASSERTED,
        _ => EVIDENCE_HOST_PIXEL,
    }
}

fn capabilities_for(tool: &str) -> Vec<&'static str> {
    match tool {
        "acos_observe_tree" | "acos_assert" => vec!["ui.tree"],
        "acos_observe" => vec!["ui.frame", "ui.screenshot"],
        "acos_tap" | "acos_text" | "acos_key" | "acos_action" => vec!["input"],
        "acos_logs" => vec!["logs"],
        "acos_reset" => vec!["reset"],
        "acos_status" => vec!["lifecycle"],
        _ => Vec::new(),
    }
}

/// Insert `engine`, `trust` and the evidence labels into a reply result.
fn label_meta(result: &mut Value, engine: &str, trust: Trust, evidence_label: &str) {
    if !result.is_object() {
        *result = json!({"value": result.take()});
    }
    let object = result.as_object_mut().expect("object");
    object.insert("engine".into(), json!(engine));
    object.insert("engine_frozen".into(), json!(engine == ENGINE_RUST_VM));
    object.insert("trust".into(), json!(trust.as_str()));
    object.insert("evidence_label".into(), json!(evidence_label));
    let labels = match engine {
        ENGINE_ART => json!({
            "frame": "host-observed",
            "tree": "guest-asserted",
            "logs": "guest-asserted",
            "input_acks": "guest-asserted",
            "state_change": "host-observed",
        }),
        ENGINE_NATIVE_CHILD => json!({
            "frame": "host-observed",
            "tree": "guest-asserted",
            "logs": "guest-asserted",
        }),
        _ => json!({
            "frame": "guest-asserted",
            "tree": "guest-asserted",
            "logs": "guest-asserted",
        }),
    };
    object.insert("evidence".into(), labels);
}

/// Add the bound frame's identity (sequence, hash, capture time) when there is
/// one. The sequence is what proves a frame is new; it is never invented.
fn label_frame(result: &mut Value, session: &ArtSession) {
    let Some((seq, md5, captured_at)) = session.frame_info() else {
        return;
    };
    let (_, buffer_generation, frame_number) =
        session.frame_identity_meta().unwrap_or((seq, None, None));
    if let Some(object) = result.as_object_mut() {
        object.entry("frame".to_string()).or_insert_with(|| {
            let mut frame = json!({
                "frame_seq": seq,
                "md5": md5,
                "captured_at": captured_at,
                // P3b: a tree-only reply carries no PNG, so its identity is the
                // C1 store counter/producer frame number, never a copied buffer.
                "buffer_generation": buffer_generation,
                "frame_number": frame_number,
            });
            if md5.is_empty() {
                // P9: this reply bound the tree to C1 **metadata**, not pixels.
                // Say so instead of implying a capture the caller never asked
                // for; a later screenshot is still checkable against
                // `buffer_generation`/`frame_number`.
                frame["pixels"] = json!(false);
                frame["identity"] = json!("C1 buffer metadata (no pixel copy)");
            }
            frame
        });
        object.insert("last_frame_seq".into(), json!(session.last_frame_seq()));
    }
}

/// Per-phase timers for one verb (W55).
///
/// `total_ms` is what the caller waited; the phase fields come from the
/// engine's own measurements around the single `acos-art` round trip and the
/// host-side absorb. Nothing here is estimated except
/// `settle_estimate_ms`, which is defined as the remainder and says so.
fn label_timing(result: &mut Value, session: &ArtSession, tool: &str, started: std::time::Instant) {
    let total_ms = started.elapsed().as_millis() as u64;
    let Some(object) = result.as_object_mut() else {
        return;
    };
    let mut timing = json!({
        "total_ms": total_ms,
        "note": "total_ms is the whole serve verb; the other fields are measured around the \
                 acos-art round trip and the host-side absorb",
    });
    match tool {
        "acos_tap" | "acos_text" | "acos_key" | "acos_action" => {
            if let Some(input) = session.last_input_timing() {
                let mut input_json = input.to_json();
                // P1: copy the guest's settle block into `timing.input` as
                // well as the reply's top-level `settle`, so either consumer
                // sees the same five phase fields.
                if let (Some(settle), Some(input_map)) =
                    (session.last_input_settle(), input_json.as_object_mut())
                {
                    for key in [
                        "mode",
                        "settled",
                        "ack_ms",
                        "first_change_ms",
                        "frame_ms",
                        "state_changed",
                        "bound",
                    ] {
                        if let Some(value) = settle.get(key) {
                            input_map.insert(key.to_string(), value.clone());
                        }
                    }
                }
                if let Some(map) = timing.as_object_mut() {
                    map.insert("input".into(), input_json);
                }
            }
        }
        "acos_observe" | "acos_observe_tree" | "acos_assert" => {
            if let Some(phase) = session.last_phase_timing() {
                if let Some(map) = timing.as_object_mut() {
                    map.insert("read".into(), phase.to_json());
                }
            }
        }
        _ => {}
    }
    object.insert("timing".into(), timing);
}

// -- verb implementations --------------------------------------------------- //

fn launch_reply(
    request_id: &str,
    id: &str,
    held: &HeldArtSession,
    session: &ArtSession,
    tree: &UiTree,
    fresh_guest: bool,
    replaced_guest: Option<Value>,
    started: std::time::Instant,
) -> Envelope {
    let (max_nodes, node_count, truncated, tree_value) = tree_json(tree, 2000);
    let mut result = json!({
        "session_id": id,
        "launched": true,
        "relaunch": !fresh_guest,
        "replaced_guest": replaced_guest,
        "activity": session.activity(),
        "apk": held.apk.display().to_string(),
        "stage_reached": "first_frame",
        "isolation": if fresh_guest { "fresh-guest-process" } else { "live-session" },
        "tree": tree_value,
        "tree_max_nodes": max_nodes,
        "tree_node_count": node_count,
        "tree_truncated": truncated,
        "events": [],
        "stages": [
            {"stage": "process", "ok": true, "status": "ok"},
            {"stage": "activity_resumed", "ok": true, "status": "ok"},
            {"stage": "first_frame", "ok": true, "status": "ok"}
        ],
    });
    label_meta(&mut result, ENGINE_ART, held.trust, EVIDENCE_HOST_PIXEL);
    result["animations"] = json!(session.animations());
    result["insets"] = json!(session.insets());
    // H13b: which boot image this session booted and whether its cache hit.
    result["boot_image"] = session
        .boot_image()
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    label_frame(&mut result, session);
    // P9: the same `settle` object the tree verbs return, so a launch says how
    // its tree was bound (`strict-event` on the P12 event rule, or
    // `probe-fresh` from the first traversal with no quiet window).
    result["settle"] = session.last_tree_settle().cloned().unwrap_or(Value::Null);
    // G3c: how the first frame was bound. A frame that is still only the window
    // skeleton is published as `first_frame.placeholder: true` *and* with
    // `frame.meaningful: false`, so an assertion can never rest on it.
    if let Some(report) = session.first_frame_report() {
        result["first_frame"] = report.to_json();
        if report.placeholder {
            if let Some(frame) = result.get_mut("frame").and_then(Value::as_object_mut) {
                frame.insert("meaningful".into(), json!(false));
                frame.insert("placeholder".into(), json!(true));
            }
        }
    }
    label_timing(&mut result, session, "acos_launch", started);
    Envelope::pass(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::ArtReference,
        Metrics::wall_ms(0),
    )
    .with_session_id(id.to_string())
    .with_value_result(result)
    .with_capabilities(["apk.inspect", "lifecycle", "ui.frame", "ui.tree"])
}

fn observe_tree(session: &mut ArtSession, args: &Value) -> Result<Value, BackendError> {
    let max_nodes = args
        .get("max_nodes")
        .and_then(Value::as_u64)
        .unwrap_or(2000) as usize;
    // P3b: the tree verb consumes P1's settle vocabulary. `strict` is the
    // default and unchanged; `fast` is for inspection/navigation, labelled in
    // the reply, and never accepted by an assertion.
    let settle = requested_settle(args)?;
    let t_begin = m5_now_us();
    let tree = session.tree_with_settle(settle)?;
    let t_tree = m5_now_us();
    let (max_nodes, node_count, truncated, tree_value) = tree_json(&tree, max_nodes);
    let t_end = m5_now_us();
    if p3_enabled() {
        p3_trace(
            "serve",
            "serve.observe_tree",
            t_begin,
            t_end,
            &format!(
                "engine_us={} tree_json_us={} nodes={} tree_bytes={}",
                t_tree.saturating_sub(t_begin),
                t_end.saturating_sub(t_tree),
                node_count,
                serde_json::to_vec(&tree_value)
                    .map(|v| v.len())
                    .unwrap_or(0)
            ),
        );
    }
    Ok(json!({
        "package": session.package(),
        "stage_reached": "first_frame",
        "windows": distinct_windows(&tree),
        "truncated": truncated,
        "max_nodes": max_nodes,
        "node_count": node_count,
        "tree": tree_value,
        "settle": session.last_tree_settle().cloned(),
    }))
}

fn observe(session: &mut ArtSession, args: &Value) -> Result<Value, BackendError> {
    let want_tree = args.get("tree").and_then(Value::as_bool).unwrap_or(false);
    let want_text = args.get("text").and_then(Value::as_bool).unwrap_or(false);
    let settle = requested_settle(args)?;
    let path = args
        .get("screenshot")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    if want_tree || want_text {
        let tree = session.tree_with_settle(settle)?;
        let mut result = json!({
            "package": session.package(),
            "stage_reached": "first_frame",
            "frame": {
                "frame_seq": tree.frame.frame_seq,
                "width": tree.frame.width,
                "height": tree.frame.height,
                "content_hash": tree.frame.content_hash,
                "meaningful": true,
            },
        });
        if want_tree {
            let (_, node_count, truncated, tree_value) = tree_json(&tree, 2000);
            result["tree"] = tree_value;
            result["truncated"] = json!(truncated);
            result["node_count"] = json!(node_count);
        }
        if want_text {
            result["text"] = json!(flat_text(&tree));
        }
        result["settle"] = session.last_tree_settle().cloned().unwrap_or(Value::Null);
        return Ok(result);
    }
    // A frame-only observe is a real, fresh host capture.
    // P2: the daemon keeps the PNG where the launcher wrote it and copies that
    // artifact to the caller's path with one kernel-side copy, instead of reading
    // 9.5 MiB into the daemon only to write the same bytes back out.
    let frame = session.capture_frame()?;
    let mut result = json!({
        "package": session.package(),
        "stage_reached": "first_frame",
        "frame": {
            "frame_seq": frame.frame_seq,
            "previous_seq": frame.previous_seq,
            "width": session.width(),
            "height": session.height(),
            "md5": frame.md5,
            "captured_at": frame.captured_at,
            "meaningful": frame.bytes > 0,
            "bytes": frame.bytes,
        },
    });
    if let Some(path) = path {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| BackendError::Error {
                code: ErrorCode::RenderIo,
                detail: format!("cannot create {}: {error}", parent.display()),
            })?;
        }
        let t_write = m5_now_us();
        std::fs::copy(&frame.path, &path).map_err(|error| BackendError::Error {
            code: ErrorCode::RenderIo,
            detail: format!(
                "cannot write {} from the frame artifact {}: {error}",
                path.display(),
                frame.path.display()
            ),
        })?;
        let t_done = m5_now_us();
        m5_trace(
            "cli.serve_shot_write",
            t_write,
            t_done,
            &format!("png_bytes={}", frame.bytes),
        );
        result["screenshot"] = json!({
            "path": path.display().to_string(),
            "bytes": frame.bytes,
            "frame_seq": frame.frame_seq,
        });
    }
    Ok(result)
}

fn tap(session: &mut ArtSession, args: &Value) -> Result<Value, BackendError> {
    let settle = requested_settle(args)?;
    // SE1: reject an unverifiable pass before the input is injected.
    reject_fast_expect_change(args)?;
    let result = if let Some(id) = args.get("id").and_then(Value::as_str) {
        let target = resolve_selector(session, &Selector::by_id(id), "id", id)?;
        session.set_settle_mode(settle);
        session.begin_input();
        let (x, y) = target.tap_point();
        session.tap_in_window(&target.window, x, y)?;
        input_result(session, "tap", json!({"id": id}))
    } else if let Some(text) = args.get("text").and_then(Value::as_str) {
        session.set_settle_mode(settle);
        session.begin_input();
        session.tap_text(text)?;
        input_result(session, "tap", json!({"text": text}))
    } else {
        let (Some(x), Some(y)) = (
            args.get("x").and_then(Value::as_i64),
            args.get("y").and_then(Value::as_i64),
        ) else {
            return Err(BackendError::Failed {
                code: ErrorCode::InvalidArgument,
                detail: "acos_tap needs `id`, `text`, or both `x` and `y`".to_string(),
            });
        };
        session.set_settle_mode(settle);
        session.begin_input();
        session.tap(x as i32, y as i32)?;
        input_result(session, "tap", json!({"x": x, "y": y}))
    };
    check_expect_change(args, session, &result)?;
    Ok(result)
}

fn type_text(session: &mut ArtSession, args: &Value) -> Result<Value, BackendError> {
    let settle = requested_settle(args)?;
    reject_fast_expect_change(args)?;
    let Some(text) = args.get("text").and_then(Value::as_str) else {
        return Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: "acos_text needs a string `text`".to_string(),
        });
    };
    // Typing needs a focused editable target. Without one there is no IME to
    // commit into, and raw key events would be delivered to whatever view the
    // window happens to focus next (the counter fixture's Increment button, for
    // example, which then also reacts to a following ENTER). That is a real
    // state mutation no agent asked for, so the engine says what is missing
    // instead of injecting it.
    let tree = session.ensure_bound_tree()?;
    if focused_editable(&tree).is_none() {
        return Err(BackendError::unsupported(
            "text.input",
            "no focused editable view in the live ART tree; typing needs an EditText \
             (tap it first). Raw keys are not injected into a non-editable view.",
        ));
    }
    session.set_settle_mode(settle);
    session.begin_input();
    session.type_text(text)?;
    let result = input_result(session, "text", json!({"text": text}));
    check_expect_change(args, session, &result)?;
    Ok(result)
}

/// The focused editable node, if any. A text-typing command needs one.
fn focused_editable(tree: &UiTree) -> Option<&UiNode> {
    fn walk(nodes: &[UiNode]) -> Option<&UiNode> {
        for node in nodes {
            if node.focused && is_editable_class(&node.class) {
                return Some(node);
            }
            if let Some(found) = walk(&node.children) {
                return Some(found);
            }
        }
        None
    }
    walk(&tree.nodes)
}

fn is_editable_class(class: &str) -> bool {
    class.ends_with("EditText")
        || class.ends_with("AutoCompleteTextView")
        || class.ends_with("MultiAutoCompleteTextView")
}

fn key(session: &mut ArtSession, args: &Value) -> Result<Value, BackendError> {
    let settle = requested_settle(args)?;
    reject_fast_expect_change(args)?;
    let code = args.get("code").cloned().unwrap_or(Value::Null);
    let value = if let Some(name) = code.as_str() {
        let name = name.to_ascii_lowercase();
        session.set_settle_mode(settle);
        session.begin_input();
        session.key(&name)?;
        json!({"key": name})
    } else if let Some(code) = code.as_i64() {
        session.set_settle_mode(settle);
        session.begin_input();
        session.key_code_i32(code as i32)?;
        json!({"code": code})
    } else {
        return Err(BackendError::unsupported(
            "input.key",
            "acos_key needs `code` as an integer keycode or a symbolic key name \
             (enter, back, tab, ...)",
        ));
    };
    let result = input_result(session, "key", value);
    check_expect_change(args, session, &result)?;
    Ok(result)
}

fn action(session: &mut ArtSession, args: &Value) -> Result<Value, BackendError> {
    let mut action = args.get("action").cloned().unwrap_or(Value::Null);
    // A top-level `settle` applies to the inner input verb as well.
    if let (Some(settle), Some(object)) = (args.get("settle"), action.as_object_mut()) {
        object.entry("settle").or_insert_with(|| settle.clone());
    }
    // SE1: so does a top-level `expect_change`.
    if let (Some(flag), Some(object)) = (args.get("expect_change"), action.as_object_mut()) {
        object.entry("expect_change").or_insert_with(|| flag.clone());
    }
    let kind = action.get("kind").and_then(Value::as_str).unwrap_or("tap");
    match kind {
        "tap" => tap(session, &action),
        "key" => key(session, &action),
        "text" => type_text(session, &action),
        other => Err(BackendError::unsupported(
            format!("input.{other}"),
            "acos_action supports tap, key and text on the ART engine",
        )),
    }
}

/// The server-side assertion over the live tree.
///
/// One round trip instead of "observe_tree then compare in the client". It is
/// a pass only when the node is found **and** its text matches; anything else is
/// a structured failure (never a quiet `ok: true`).
fn assert(session: &mut ArtSession, args: &Value, trust: Trust) -> Result<Value, BackendError> {
    // P1: an assertion is strict by construction. A caller may pass
    // `settle:"strict"` (redundant); `settle:"fast"` is rejected rather than
    // silently ignored, because fast frames may be mid-animation.
    match args.get("settle").and_then(Value::as_str) {
        None | Some("strict") => {}
        Some("fast") => {
            return Err(BackendError::Failed {
                code: ErrorCode::InvalidArgument,
                detail: "acos_assert always uses strict settle: fast frames may be                          mid-animation and can never justify an assertion"
                    .to_string(),
            })
        }
        Some(other) => {
            return Err(BackendError::Failed {
                code: ErrorCode::InvalidArgument,
                detail: format!(
                    "settle must be \"fast\" or \"strict\" (got {other:?}); assertions only accept strict"
                ),
            })
        }
    }
    if trust == Trust::Untrusted {
        return Err(BackendError::Failed {
            code: ErrorCode::InsufficientHostEvidence,
            detail: "acos_assert cannot pass on an untrusted run (guest-asserted tree)".to_string(),
        });
    }
    let mode = args.get("mode").and_then(Value::as_str).unwrap_or("exact");
    let expect = args
        .get("expect")
        .or_else(|| args.get("text"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let tree = session.tree()?;
    // P12b: a settle timeout is an observation, not evidence. Assertions stay
    // strict: the tree may be mid-animation, so it can never justify a pass.
    if let Some(settle) = session.last_tree_settle() {
        if settle.get("settled").and_then(Value::as_bool) == Some(false) {
            return Err(BackendError::Failed {
                code: ErrorCode::Timeout,
                detail: format!(
                    "acos_assert uses strict settle and the strict observation timed out \
                     (bound:{}, reason:{}); a timeout is never a pass. Use acos_observe_tree \
                     with settle:\"fast\" or settle_timeout_ms to inspect the screen instead.",
                    settle
                        .get("bound")
                        .and_then(Value::as_str)
                        .unwrap_or("timeout"),
                    settle
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                ),
            });
        }
    }
    let (selector, describe) = if let Some(id) = args.get("id").and_then(Value::as_str) {
        (Selector::by_id(id), json!({"id": id}))
    } else if let Some(text) = args.get("text").and_then(Value::as_str) {
        (Selector::by_text(text), json!({"text": text}))
    } else {
        return Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: "acos_assert needs `id` (or `text`) to select the node".to_string(),
        });
    };
    let node = tree
        .find_one(&selector)
        .ok_or_else(|| BackendError::Failed {
            code: if tree.truncated {
                ErrorCode::TreeTruncated
            } else {
                ErrorCode::ElementNotFound
            },
            detail: format!(
                "no node matching {} in the live ART tree{}",
                describe,
                if tree.truncated {
                    " (the tree was truncated: absence is not proven)"
                } else {
                    ""
                }
            ),
        })?;
    let actual = node.text.clone();
    let matched = match (mode, expect.as_deref()) {
        ("contains", Some(expect)) => actual.contains(expect),
        ("regex", Some(expect)) => match regex_lite_matches(expect, &actual) {
            Ok(matched) => matched,
            Err(detail) => {
                return Err(BackendError::Failed {
                    code: ErrorCode::InvalidArgument,
                    detail,
                })
            }
        },
        ("exact" | _, Some(expect)) => actual.trim() == expect,
        (_, None) => true,
    };
    if !matched {
        return Err(BackendError::Failed {
            code: ErrorCode::AssertionFailed,
            detail: format!(
                "assert {} ({}): text {:?} does not match expected {:?}",
                describe,
                mode,
                actual,
                expect.unwrap_or_default()
            ),
        });
    }
    Ok(json!({
        "assert": true,
        "mode": mode,
        "selector": describe,
        "text": actual,
        "expect": expect,
        "resource_id": node.resource_id,
        "bounds": node.bounds.to_string(),
        "tree_truncated": tree.truncated,
    }))
}

/// A tiny, dependency-free exact/contains fallback. `regex` is delegated to
/// `acos_protocol`'s selector engine by matching a synthetic tree; a missing
/// regex crate link here is not worth a new dependency.
fn regex_lite_matches(pattern: &str, text: &str) -> Result<bool, String> {
    let tree = UiTree::new(
        acos_protocol::FrameMeta::new(0, String::new(), "", 0, 0, 0, "", true),
        "",
        vec![UiNode::new("n", "", Bounds::new(0, 0, 0, 0)).with_text(text)],
    );
    let selector = Selector {
        text: Some(acos_protocol::ui::TextMatch {
            regex: Some(pattern.to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    Ok(tree.find_one(&selector).is_some())
}

fn logs(session: &mut ArtSession) -> Result<Value, BackendError> {
    let records = session.logs_since(0)?;
    let crash = session.last_crash();
    let lines: Vec<Value> = records
        .iter()
        .map(|record| {
            let priority = match record.level {
                LogLevel::Verbose => "V",
                LogLevel::Debug => "D",
                LogLevel::Info => "I",
                LogLevel::Warn => "W",
                LogLevel::Error => "E",
            };
            json!({
                "seq": record.seq,
                "priority": priority,
                "tag": record.tag,
                "message": record.message,
                "line": format!("{} {:<8} {}", priority, record.tag, record.message),
                "repeat_count": 1,
            })
        })
        .collect();
    Ok(json!({
        "launched": true,
        "stage_reached": "first_frame",
        "total": records.len(),
        "count": records.len(),
        "collapsed": 0,
        "logs": lines,
        "crash": crash.map(|crash| json!({
            "exception": crash.exception,
            "message": crash.message,
            "fatal": crash.fatal,
        })),
        "note": if records.is_empty() {
            "the guest logd sink is empty; the app reached first_frame, so this is an empty sink, not a missing one"
        } else {
            ""
        },
    }))
}

fn status_value(session: &ArtSession) -> Value {
    json!({
        "launched": true,
        "package": session.package(),
        "activity": session.activity(),
        "apk": session.apk_path().display().to_string(),
        "stage_reached": "first_frame",
        "published_frames": session.frame_info().map(|f| f.0).unwrap_or(0),
        "live": true,
        "mode": "live",
        "animations": session.animations(),
        "insets": session.insets(),
        "capabilities": {
            "tree": true, "screenshot": true, "tap": true, "tap_text": true,
            "type_text": true, "key": true, "reset": true, "logs": true,
            "assert": true, "swipe": false, "environment": false,
        },
        "unsupported": ["acos_swipe"],
    })
}

fn reset(session: &mut ArtSession, args: &Value) -> Result<Value, BackendError> {
    // P9: reset consumes the same settle vocabulary as launch. `strict` is the
    // default and unchanged (150 ms quiet window + capture); `fast` binds the
    // relaunched guest's first traversal from a fresh probe sample, with no
    // quiet window and no PNG.
    let (settle, pixels) = launch_bind(args)?;
    let before = session.last_frame_seq();
    session.set_reset_bind(settle, pixels, settle_timeout_arg(args));
    session.reset()?;
    let tree = session.ensure_bound_tree()?;
    // P9: the reset reply carries the relaunched app's bound tree, so "reset ->
    // first correct screen" is one round trip (the tree was already bound by
    // reset; serializing it costs ~1 ms). Same shape as the launch reply.
    let (max_nodes, node_count, truncated, tree_value) = tree_json(&tree, 2000);
    let mut reply = json!({
        "state_reset": true,
        "data_cleared": true,
        "scope": "all",
        "isolation": "fresh-guest-process",
        "stage_reached": "first_frame",
        "previous_frame_seq": before,
        "frame_seq": tree.frame.frame_seq,
        "package": session.package(),
        "tree": tree_value,
        "tree_max_nodes": max_nodes,
        "tree_node_count": node_count,
        "tree_truncated": truncated,
        "settle": session.last_tree_settle().cloned().unwrap_or(Value::Null),
        "note": "the guest process was stopped and the same APK relaunched with empty app-data",
    });
    // S3b: never a silent retry. The first host-transport failure and the path
    // to its preserved evidence travel with the successful reset reply.
    if let Some(retry) = session.relaunch_retry() {
        reply["relaunch_retry"] = retry.clone();
    }
    Ok(reply)
}

/// P1 part 3: `observe: {tree:true, png:false}` on an input verb.
///
/// The acos-art input reply already carries the settled tree and the bound C1
/// frame; `absorb` caches both. Attaching the cached tree here returns ack +
/// changed tree + frame reference in the **same** serve round trip, with no
/// extra traversal. The frame reference itself is added by `label_frame`.
fn attach_observation(
    session: &mut ArtSession,
    result: &mut Value,
    args: &Value,
) -> Result<(), BackendError> {
    let Some(observe) = args.get("observe") else {
        return Ok(());
    };
    let Some(observe) = observe.as_object() else {
        return Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: "observe must be an object, for example {\"tree\":true,\"png\":false}"
                .to_string(),
        });
    };
    let want_tree = observe
        .get("tree")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let want_png = observe.get("png").and_then(Value::as_bool).unwrap_or(false);
    if want_png {
        return Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: "observe.png is not available on an input verb; the reply's `frame` \
                     reference is returned and `acos_observe` serves the PNG"
                .to_string(),
        });
    }
    if !want_tree {
        return Ok(());
    }
    let max_nodes = observe
        .get("max_nodes")
        .and_then(Value::as_u64)
        .unwrap_or(2000) as usize;
    let tree = session.ensure_bound_tree()?;
    let (max_nodes, node_count, truncated, tree_value) = tree_json(&tree, max_nodes);
    if let Some(object) = result.as_object_mut() {
        object.insert("tree".into(), tree_value);
        object.insert("tree_max_nodes".into(), json!(max_nodes));
        object.insert("tree_node_count".into(), json!(node_count));
        object.insert("tree_truncated".into(), json!(truncated));
        object.insert(
            "observe".into(),
            json!({"tree": true, "png": false, "round_trip": "same-acos-art-reply"}),
        );
    }
    Ok(())
}

/// P9: the settle mode and pixel capture a launch / reset reply binds with.
///
/// `settle` reuses P1's vocabulary (`strict` default, `fast` opt-in). `pixels`
/// defaults to **false**: a launch reply needs the bound frame's *identity*
/// (P3b's C1 metadata: `buffer_generation`, `frame_number`), not a 9.5 MiB PNG
/// nobody asked for. `pixels:true` keeps the old capture reply, and is refused
/// together with `settle:"fast"` because a fast frame may be mid-animation and
/// must never back a pixel claim.
fn launch_bind(args: &Value) -> Result<(&'static str, bool), BackendError> {
    let settle = requested_settle(args)?;
    let pixels = match args.get("pixels") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(want)) => *want,
        Some(_) => {
            return Err(BackendError::Failed {
                code: ErrorCode::InvalidArgument,
                detail: "`pixels` must be a boolean (default false: the reply carries C1 frame \
                         identity, not a PNG)"
                    .to_string(),
            })
        }
    };
    if settle == "fast" && pixels {
        return Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: "settle:fast is a tree-only mode; a fast frame may be mid-animation and \
                     cannot back a PNG capture (pass pixels:false, or settle:strict)"
                .to_string(),
        });
    }
    Ok((settle, pixels))
}

fn requested_settle(args: &Value) -> Result<&'static str, BackendError> {
    match args.get("settle").and_then(Value::as_str) {
        None | Some("strict") => Ok("strict"),
        Some("fast") => Ok("fast"),
        Some(other) => Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: format!("settle must be \"fast\" or \"strict\" (got {other:?})"),
        }),
    }
}

/// P12b: `settle_timeout_ms` (integer, 1..=600000). `None` keeps the engine's
/// documented default. On timeout the guest still answers with the latest tree
/// and frame identity and `settled:false`, so the agent can use what is on
/// screen; assertions and `verify` never count that as a pass.
fn settle_timeout_arg(args: &Value) -> Option<u64> {
    args.get("settle_timeout_ms")
        .and_then(Value::as_u64)
        .map(|ms| ms.clamp(1, 600_000))
}

fn input_result(session: &ArtSession, kind: &str, target: Value) -> Value {
    let delivery = session.last_input_delivery();
    let settle = session.last_input_settle().cloned();
    let (delivered, state_changed, acks) = match delivery {
        Some(delivery) => (
            delivery.delivered,
            delivery.visual_change,
            json!(delivery
                .acks
                .iter()
                .map(|ack| json!({
                    "seq": ack.seq,
                    "handled": ack.handled,
                    "latency_us": ack.latency_us,
                }))
                .collect::<Vec<_>>()),
        ),
        None => (true, false, json!([])),
    };
    json!({
        "action": kind,
        "accepted": delivered,
        "state_changed": state_changed,
        "acks": acks,
        "target": target,
        "settle": settle,
        "input_evidence": "guest-asserted acks; state_changed is the host's own pixel+tree comparison in strict mode; in fast mode it is the guest's tree-change verdict and a fast frame may be mid-animation",
    })
}

/// SE1: parse the per-action `expect_change` flag. Strict-only, exactly like the
/// scenario step flag; a non-bool is an argument error, never ignored.
fn expect_change_arg(args: &Value) -> Result<bool, BackendError> {
    match args.get("expect_change") {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(other) => Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: format!("`expect_change` must be a boolean (got {other})"),
        }),
    }
}

/// SE1: `expect_change: true` with `settle: fast` would be an unverifiable pass
/// (a fast frame may be mid-animation and is never verification evidence), so
/// it is rejected before any input is injected.
fn reject_fast_expect_change(args: &Value) -> Result<(), BackendError> {
    if expect_change_arg(args)?
        && matches!(args.get("settle").and_then(Value::as_str), Some("fast"))
    {
        return Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: "`expect_change: true` cannot be combined with `settle: fast`: a fast frame \
                     may be mid-animation and is never verification evidence; use `settle: \
                     strict` (the default)"
                .to_string(),
        });
    }
    Ok(())
}

/// SE1: enforce `expect_change: true` on a per-action reply.
///
/// The rule reads the same fact the reply carries (`settle.pixels_changed`): a
/// settled action that changed no pixel fails with `ASSERTION_FAILED` and
/// `details.reason = "expect_change"`. A reply with no measured pixel fact is
/// `UNSUPPORTED_ASSERTION` - an unmeasured change never passes.
fn check_expect_change(
    args: &Value,
    session: &ArtSession,
    result: &Value,
) -> Result<(), BackendError> {
    if !expect_change_arg(args)? {
        return Ok(());
    }
    let settle = result.get("settle").filter(|settle| !settle.is_null());
    let pixels_changed = settle
        .and_then(|settle| settle.get("pixels_changed"))
        .and_then(Value::as_bool);
    if pixels_changed == Some(true) {
        return Ok(());
    }
    let after_frame_seq = session.last_frame_seq();
    let details = json!({
        "reason": "expect_change",
        "pixels_changed": pixels_changed,
        "frame_seq": after_frame_seq,
        "settle": settle.cloned().unwrap_or(Value::Null),
    });
    if pixels_changed == Some(false) {
        return Err(BackendError::Protocol {
            code: ErrorCode::AssertionFailed,
            detail: format!(
                "the settled frame is pixel-identical to the pre-action frame \
                 (`settle.pixels_changed:false`); the action proved no visual change"
            ),
            hint: Some(
                "The input was delivered and settled, but nothing on screen changed, so anything \
                 asserted afterwards would describe state this action never produced (GR4 G4c). \
                 Target the control that really writes the state, or drop `expect_change`."
                    .to_string(),
            ),
            details: Some(details),
        });
    }
    Err(BackendError::Protocol {
        code: ErrorCode::UnsupportedAssertion,
        detail: "`expect_change` cannot be verified: the reply carries no measured \
                 `settle.pixels_changed`"
            .to_string(),
        hint: Some(
            "A pixel-change claim needs a strict settle record that measures the pixels. Use \
             `settle: strict` (the default) on a backend that reports `pixels_changed`, or drop \
             `expect_change`; an unmeasured change never passes."
                .to_string(),
        ),
        details: Some(details),
    })
}

fn resolve_selector(
    session: &mut ArtSession,
    selector: &Selector,
    field: &str,
    value: &str,
) -> Result<UiNode, BackendError> {
    let tree = session.ensure_bound_tree()?;
    if let Some(node) = tree.find_one(selector) {
        return Ok(node.clone());
    }
    if let Some(offscreen) = tree.find_invisible(selector) {
        return Err(BackendError::Failed {
            code: ErrorCode::ElementOffscreen,
            detail: format!(
                "{field}={value:?} matches only a node that is not visible to the user (text {:?}, bounds {}, visible_bounds {}); scroll it into view first",
                offscreen.text,
                offscreen.bounds,
                offscreen.visible_bounds
                    .map_or_else(|| "<none>".to_string(), |b| b.to_string())
            ),
        });
    }
    tree.find_one(selector)
        .cloned()
        .ok_or_else(|| BackendError::Failed {
            code: if tree.truncated {
                ErrorCode::TreeTruncated
            } else {
                ErrorCode::ElementNotFound
            },
            detail: format!(
                "no node with {field}={value:?} in the live ART tree{}",
                if tree.truncated {
                    " (the tree was truncated: absence is not proven)"
                } else {
                    ""
                }
            ),
        })
}

// -- tree serialization ----------------------------------------------------- //

/// Serialize a live tree for the agent-facing `acos_observe_tree` result.
///
/// `max_nodes` is a hard host-side cap: the walk stops at the cap and the reply
/// says `truncated: true`, so absence is never claimed from an incomplete tree.
fn tree_json(tree: &UiTree, max_nodes: usize) -> (usize, usize, bool, Value) {
    let max_nodes = max_nodes.max(1);
    let mut budget = max_nodes;
    let mut count = 0usize;
    let mut truncated = tree.truncated;
    let roots: Vec<Value> = tree
        .nodes
        .iter()
        .filter_map(|node| node_json(node, &mut budget, &mut count, &mut truncated))
        .collect();
    (
        max_nodes,
        count,
        truncated,
        json!({
            "max_nodes": max_nodes,
            "truncated": truncated,
            "nodes": roots,
        }),
    )
}

fn node_json(
    node: &UiNode,
    budget: &mut usize,
    count: &mut usize,
    truncated: &mut bool,
) -> Option<Value> {
    if *budget == 0 {
        *truncated = true;
        return None;
    }
    *budget -= 1;
    *count += 1;
    let mut children = Vec::new();
    for child in &node.children {
        match node_json(child, budget, count, truncated) {
            Some(value) => children.push(value),
            None => break,
        }
    }
    Some(json!({
        "id": node.id,
        "class": node.class,
        "resource_id": node.resource_id,
        "text": node.text,
        "hint": node.hint,
        "content_desc": node.content_desc,
        "bounds": node.bounds.to_string(),
        "visible_bounds": node.visible_bounds.map(|b| b.to_string()),
        "visible": node.visible,
        "enabled": node.enabled,
        "focused": node.focused,
        "clickable": node.clickable,
        "scrollable": node.scrollable,
        "checked": node.checked,
        "selected": node.selected,
        "window": node.window,
        "package": node.package,
        "children": children,
    }))
}

fn distinct_windows(tree: &UiTree) -> usize {
    fn collect(node: &UiNode, seen: &mut Vec<String>) {
        if !node.window.is_empty() && !seen.contains(&node.window) {
            seen.push(node.window.clone());
        }
        for child in &node.children {
            collect(child, seen);
        }
    }
    let mut windows = Vec::new();
    for node in &tree.nodes {
        collect(node, &mut windows);
    }
    windows.len()
}

fn flat_text(tree: &UiTree) -> String {
    fn walk(node: &UiNode, depth: usize, out: &mut String) {
        let text = if !node.text.is_empty() {
            format!(" {:?}", node.text)
        } else if !node.hint.is_empty() {
            format!(" hint={:?}", node.hint)
        } else {
            String::new()
        };
        out.push_str(&"  ".repeat(depth));
        out.push_str(&node.class);
        if !node.resource_id.is_empty() {
            out.push('#');
            out.push_str(&node.resource_id);
        }
        out.push_str(&text);
        if node.clickable {
            out.push_str(" clickable");
        }
        out.push('\n');
        for child in &node.children {
            walk(child, depth + 1, out);
        }
    }
    let mut out = String::new();
    for node in &tree.nodes {
        walk(node, 0, &mut out);
    }
    out
}

// -- engine selection and error envelopes ----------------------------------- //

/// Parse `args.engine`. `Ok(None)` means "not given" (the daemon default).
///
/// No aliases and no guessing: anything but `art` / `rust-vm` is an error.
pub fn parse_engine_arg(args: &Value) -> Result<Option<Engine>, String> {
    match args.get("engine") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Engine::parse(value).map(Some).ok_or_else(|| {
            format!(
                "unknown engine {value:?}; supported engines are `art` (default, real ART) and \
                 `rust-vm` (frozen). No silent fallback is performed."
            )
        }),
        Some(other) => Err(format!(
            "engine must be a string (`art` or `rust-vm`), got {other}"
        )),
    }
}

/// The daemon's `--engine` value; an unknown value is refused at startup.
pub fn parse_engine_flag(value: &str) -> Result<Engine, String> {
    Engine::parse(value).ok_or_else(|| {
        format!("unknown --engine {value:?}; use `art` (default) or `rust-vm` (frozen)")
    })
}

fn unsupported_engine(request_id: &str, message: &str, default_engine: Engine) -> Envelope {
    art_failure(
        request_id,
        None,
        Trust::Trusted,
        ErrorCode::UnsupportedBackend,
        message.to_string(),
        json!({
            "supported_engines": ["art", "rust-vm"],
            "default_engine": default_engine.as_str(),
            "fallback_used": false,
        }),
    )
}

/// Rebuild a failure envelope with a situation-specific hint.
///
/// `Envelope` has no mutable hint accessor, so the envelope is reconstructed
/// from its own fields; no status or evidence field is touched.
fn with_hint(envelope: Envelope, hint: &str) -> Envelope {
    let Some(error) = envelope.error() else {
        return envelope;
    };
    let error = error.clone().with_hint(hint);
    let mut rebuilt = Envelope::failure(
        envelope.request_id().to_string(),
        ProtocolBackend::AcosNative,
        envelope.fidelity(),
        error,
        envelope.metrics(),
    );
    if let Some(id) = envelope.session_id() {
        rebuilt = rebuilt.with_session_id(id.to_string());
    }
    rebuilt
}

/// An ART failure with the engine/trust/evidence labels attached.
fn art_failure(
    request_id: &str,
    session_id: Option<&str>,
    trust: Trust,
    code: ErrorCode,
    message: impl Into<String>,
    details: Value,
) -> Envelope {
    let mut details = details;
    if !details.is_object() {
        details = json!({"value": details});
    }
    {
        let object = details.as_object_mut().expect("object");
        object.insert("engine".into(), json!(ENGINE_ART));
        object.insert("engine_frozen".into(), json!(false));
        object.insert("trust".into(), json!(trust.as_str()));
        object.insert("evidence_label".into(), json!(EVIDENCE_NO_GUEST));
    }
    let error = ProtocolError::new(code, message).with_details(details);
    let envelope = Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::ArtReference,
        error,
        Metrics::wall_ms(0),
    );
    match session_id {
        Some(id) => envelope.with_session_id(id.to_string()),
        None => envelope,
    }
}

/// G3f1: the launch failure plus the evidence the dead guest left behind.
///
/// The classification is the point: a reply that says only `GUEST_EXITED` is not
/// diagnosable (E13.8 lost one launch that way). `details.launch_failure` names
/// the class, the exit status/signal, the last logd lines and the artifact paths
/// that were kept; the hint says where the rest is.
fn art_error_with_launch_evidence(
    request_id: &str,
    session_id: Option<&str>,
    trust: Trust,
    error: &BackendError,
    evidence: Value,
) -> Envelope {
    let protocol = error.to_protocol_error();
    let mut details = protocol.details.clone().unwrap_or_else(|| json!({}));
    if !details.is_object() {
        details = json!({"detail": details});
    }
    {
        let object = details.as_object_mut().expect("object");
        object.insert("engine".into(), json!(ENGINE_ART));
        object.insert("engine_frozen".into(), json!(false));
        object.insert("trust".into(), json!(trust.as_str()));
        object.insert("evidence_label".into(), json!(EVIDENCE_NO_GUEST));
        object.insert("launch_failure".into(), evidence.clone());
    }
    let classification = evidence
        .get("classification")
        .cloned()
        .unwrap_or(Value::Null);
    let class = classification
        .get("class")
        .and_then(Value::as_str)
        .unwrap_or("unclassified-guest-death");
    let reason = classification
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("the guest died before its first frame");
    let kept = evidence
        .get("kept_path")
        .and_then(Value::as_str)
        .unwrap_or("(not written)");
    let hint = format!(
        "{class}: {reason}. Evidence kept under the session's artifacts: {kept} \
         (session-exit.json, the logd tail, stdout/stderr tails and any native-crash record \
         are listed in launch_failure.evidence_files). This launch produced no first frame; a \
         placeholder is never substituted for it."
    );
    let mut failure = ProtocolError::new(protocol.code, protocol.message.clone())
        .with_details(details)
        .with_hint(hint);
    failure.retryable = protocol.retryable;
    let envelope = Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::ArtReference,
        failure,
        Metrics::wall_ms(0),
    );
    match session_id {
        Some(id) => envelope.with_session_id(id.to_string()),
        None => envelope,
    }
}

fn art_error(
    request_id: &str,
    session_id: Option<&str>,
    trust: Trust,
    error: &BackendError,
) -> Envelope {
    let protocol = error.to_protocol_error();
    let mut details = protocol.details.clone().unwrap_or_else(|| json!({}));
    if !details.is_object() {
        details = json!({"detail": details});
    }
    {
        let object = details.as_object_mut().expect("object");
        object.insert("engine".into(), json!(ENGINE_ART));
        object.insert("engine_frozen".into(), json!(false));
        object.insert("trust".into(), json!(trust.as_str()));
        object.insert("evidence_label".into(), json!(EVIDENCE_NO_GUEST));
    }
    let is_frame_error = protocol.code == ErrorCode::NoFreshFrame;
    let mut error = ProtocolError::new(protocol.code, protocol.message.clone())
        .with_details(details)
        .with_hint(protocol.hint.clone());
    if !is_frame_error {
        error.retryable = protocol.retryable;
    }
    let envelope = Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::ArtReference,
        error,
        Metrics::wall_ms(0),
    );
    match session_id {
        Some(id) => envelope.with_session_id(id.to_string()),
        None => envelope,
    }
}

/// Rebuild a legacy envelope with the engine/trust/evidence labels injected.
fn relabel_legacy(envelope: Envelope, engine: &str, trust: Trust, frozen: bool) -> Envelope {
    let request_id = envelope.request_id().to_string();
    let metrics = envelope.metrics();
    let session_id = envelope.session_id().map(str::to_string);
    let capabilities = envelope.capabilities_used().to_vec();
    let frame = envelope.frame().cloned();
    let mut labeled = if envelope.ok() {
        let mut result = envelope.result().cloned().unwrap_or_else(|| json!({}));
        label_meta(&mut result, engine, trust, EVIDENCE_NO_GUEST);
        if let Some(object) = result.as_object_mut() {
            object.insert("engine_frozen".into(), json!(frozen));
            object.insert(
                "note".into(),
                json!(if frozen {
                    "the frozen Rust VM produced this reply; it is not real ART"
                } else {
                    "the confined native child produced this reply"
                }),
            );
        }
        Envelope::pass(
            request_id,
            ProtocolBackend::AcosNative,
            envelope.fidelity(),
            metrics,
        )
        .with_value_result(result)
    } else {
        let mut error = envelope
            .error()
            .cloned()
            .unwrap_or_else(|| ProtocolError::new(ErrorCode::Internal, "daemon error"));
        let mut details = error.details.clone().unwrap_or_else(|| json!({}));
        if !details.is_object() {
            details = json!({"detail": details});
        }
        {
            let object = details.as_object_mut().expect("object");
            object.insert("engine".into(), json!(engine));
            object.insert("engine_frozen".into(), json!(frozen));
            object.insert("trust".into(), json!(trust.as_str()));
            object.insert("evidence_label".into(), json!(EVIDENCE_GUEST_ASSERTED));
        }
        error.details = Some(details);
        Envelope::failure(
            request_id,
            ProtocolBackend::AcosNative,
            envelope.fidelity(),
            error,
            metrics,
        )
    };
    if let Some(id) = session_id {
        labeled = labeled.with_session_id(id);
    }
    if let Some(frame) = frame {
        labeled = labeled.with_frame(frame);
    }
    if !capabilities.is_empty() {
        labeled = labeled.with_capabilities(capabilities);
    }
    labeled
}

fn with_result(envelope: Envelope, result: Value) -> Envelope {
    let request_id = envelope.request_id().to_string();
    let metrics = envelope.metrics();
    let mut rebuilt = Envelope::pass(
        request_id,
        ProtocolBackend::AcosNative,
        envelope.fidelity(),
        metrics,
    )
    .with_value_result(result);
    if let Some(id) = envelope.session_id() {
        rebuilt = rebuilt.with_session_id(id.to_string());
    }
    if !envelope.capabilities_used().is_empty() {
        rebuilt = rebuilt.with_capabilities(envelope.capabilities_used().to_vec());
    }
    rebuilt
}

fn trust_arg(args: &Value) -> Trust {
    match args.get("trust").and_then(Value::as_str) {
        Some("untrusted") => Trust::Untrusted,
        _ => Trust::Trusted,
    }
}

fn request_id(args: &Value) -> String {
    args.pointer("/base/request_id")
        .and_then(Value::as_str)
        .unwrap_or("req_daemon")
        .to_string()
}

fn session_id(args: &Value) -> String {
    match args.pointer("/base/session_id").and_then(Value::as_str) {
        Some(id) => id.to_string(),
        None => acos_mcp::session_backend::default_session_id().to_string(),
    }
}

/// Session ids become directory names, so keep them path-safe.
fn sanitize_session_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').to_string();
    if cleaned.is_empty() {
        "session".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_protocol::ui::Bounds;

    fn node(id: &str, text: &str, children: Vec<UiNode>) -> UiNode {
        let mut node = UiNode::new("n0", "android.widget.TextView", Bounds::new(0, 0, 10, 10))
            .with_resource_id(id)
            .with_text(text);
        node.children = children;
        node
    }

    fn tree(nodes: Vec<UiNode>) -> UiTree {
        UiTree::new(
            acos_protocol::FrameMeta::new(1, String::new(), "", 1080, 2400, 420, "", true),
            "dev.acos.fixtures.counter",
            nodes,
        )
    }

    #[test]
    fn engine_argument_is_explicit_and_unknown_engines_are_refused() {
        assert_eq!(parse_engine_arg(&json!({})).unwrap(), None);
        assert_eq!(
            parse_engine_arg(&json!({"engine": "art"})).unwrap(),
            Some(Engine::Art)
        );
        assert_eq!(
            parse_engine_arg(&json!({"engine": "rust-vm"})).unwrap(),
            Some(Engine::RustVm)
        );
        let err = parse_engine_arg(&json!({"engine": "artx"})).unwrap_err();
        assert!(err.contains("unknown engine"), "{err}");
        assert!(err.contains("no silent fallback") || err.contains("No silent fallback"));
        assert!(parse_engine_arg(&json!({"engine": 7})).is_err());
        assert_eq!(parse_engine_flag("art").unwrap(), Engine::Art);
        assert_eq!(parse_engine_flag("rust-vm").unwrap(), Engine::RustVm);
        assert!(parse_engine_flag("native").is_err());
    }

    #[test]
    fn tree_json_caps_nodes_and_reports_truncation() {
        let child = node("count", "1", vec![]);
        let root = node("", "", vec![child]);
        let (max, count, truncated, value) = tree_json(&tree(vec![root]), 1);
        assert_eq!(max, 1);
        assert_eq!(count, 1, "the cap is a hard node budget");
        assert!(truncated, "a capped tree must say so");
        assert_eq!(value["nodes"].as_array().unwrap().len(), 1);
        let (_, _, truncated, value) = tree_json(&tree(vec![node("inc", "0", vec![])]), 2000);
        assert!(!truncated);
        assert_eq!(value["nodes"][0]["resource_id"], json!("inc"));
        assert_eq!(value["nodes"][0]["bounds"], json!("[0,0][10,10]"));
        assert!(value["nodes"][0]["children"].as_array().unwrap().is_empty());
    }

    #[test]
    fn frozen_label_and_default_engine_are_distinct_strings() {
        // The gate W50 exists for: a caller can tell the frozen Rust VM apart
        // from real ART in every reply.
        assert_eq!(ENGINE_ART, "art");
        assert_eq!(ENGINE_RUST_VM, "rust-vm, frozen");
        assert_ne!(ENGINE_ART, ENGINE_RUST_VM);
    }

    #[test]
    fn session_ids_are_path_safe() {
        assert_eq!(sanitize_session_id("s_w50_counter"), "s_w50_counter");
        assert_eq!(sanitize_session_id("../../etc/passwd"), "_.._etc_passwd");
        assert!(!sanitize_session_id("../../etc/passwd").contains('/'));
        assert!(!sanitize_session_id("../..").contains('/'));
        assert_eq!(sanitize_session_id(""), "session");
    }

    #[test]
    fn labels_are_written_into_results_and_errors() {
        let mut result = json!({"ok": true});
        label_meta(
            &mut result,
            ENGINE_RUST_VM,
            Trust::Untrusted,
            EVIDENCE_NO_GUEST,
        );
        assert_eq!(result["engine"], json!(ENGINE_RUST_VM));
        assert_eq!(result["engine_frozen"], json!(true));
        assert_eq!(result["trust"], json!("untrusted"));
        assert_eq!(result["evidence"]["tree"], json!("guest-asserted"));

        let failure = art_failure(
            "req",
            Some("s"),
            Trust::Trusted,
            ErrorCode::UnsupportedBackend,
            "no swipe",
            json!({"verb": "acos_swipe"}),
        );
        assert!(!failure.ok());
        let details = failure.error().unwrap().details.clone().unwrap();
        assert_eq!(details["engine"], json!(ENGINE_ART));
        assert_eq!(details["trust"], json!("trusted"));
        assert_eq!(details["verb"], json!("acos_swipe"));
    }

    /// P9: launch/reset bind with P1's vocabulary, are pixel-free by default,
    /// and refuse the one combination that would let a mid-animation frame back
    /// a pixel claim.
    #[test]
    fn launch_bind_defaults_to_strict_pixel_free_and_refuses_fast_pixels() {
        assert_eq!(launch_bind(&json!({})).unwrap(), ("strict", false));
        assert_eq!(
            launch_bind(&json!({"settle": "strict", "pixels": true})).unwrap(),
            ("strict", true)
        );
        assert_eq!(
            launch_bind(&json!({"settle": "fast"})).unwrap(),
            ("fast", false)
        );
        // Unknown settle values are refused, never coerced to fast.
        let err = launch_bind(&json!({"settle": "quick"})).unwrap_err();
        assert!(err.to_string().contains("settle must be"), "{err}");
        // A non-boolean `pixels` is a typed argument error.
        let err = launch_bind(&json!({"pixels": "yes"})).unwrap_err();
        assert!(
            err.to_string().contains("`pixels` must be a boolean"),
            "{err}"
        );
        // fast + pixels would be a pixel claim from a possibly mid-animation
        // frame; the launcher refuses it and so does serve.
        let err = launch_bind(&json!({"settle": "fast", "pixels": true})).unwrap_err();
        assert!(
            err.to_string().contains("tree-only mode"),
            "fast+pixels must be refused, got {err}"
        );
    }
}
