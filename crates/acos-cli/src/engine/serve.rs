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
use std::path::{Path, PathBuf};
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
use super::zygote_host::ZygoteHost;
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
    /// SERVELEAK2/F3: the identity of the **launch** that currently owns this
    /// session id. Every `acos_launch` renews it, `session_list` reports it and a
    /// client that launched the session records it, so a client whose launch was
    /// replaced by another one can tell that the session is no longer its guest
    /// (and must not destroy it on exit).
    launch_token: Mutex<String>,
    /// `Mutex` because two connections may address the same session id; the
    /// guest takes one command at a time.
    session: Mutex<ArtSession>,
}

impl HeldArtSession {
    /// Renew and return this session's launch token (SERVELEAK2/F3).
    fn renew_launch_token(&self) -> String {
        let token = new_launch_token();
        *self.launch_token.lock().unwrap_or_else(|e| e.into_inner()) = token.clone();
        token
    }

    fn launch_token(&self) -> String {
        self.launch_token
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// A fresh per-launch identity (SERVELEAK2/F3).
fn new_launch_token() -> String {
    let mut bytes = [0u8; 8];
    let read = std::fs::File::open("/dev/urandom")
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut bytes))
        .is_ok();
    if !read {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        bytes.copy_from_slice(&nanos.to_le_bytes());
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
    /// ZPROD3: the daemon owns the persistent zygote keepers (`ACOS_ART_ZYGOTE=on`).
    /// One keeper per `ZygoteKey`, closed on serve exit: a keeper outlives
    /// sessions, never a serve.
    zygote: Arc<ZygoteHost>,
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
        // ZPROD3: keepers live inside the daemon's private runtime directory.
        let zygote = Arc::new(ZygoteHost::for_artifacts(&artifacts_root));
        ServeEngine {
            legacy,
            default_engine,
            max_sessions: max_sessions.max(1),
            artifacts_root,
            live: crate::live_registry::LiveRegistry::from_env(),
            art: Mutex::new(HashMap::new()),
            routes: Mutex::new(HashMap::new()),
            zygote,
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
        // ZPROD3 (design §3/§6): the keeper outlives sessions, never a serve.
        // Closing every keeper ends the zygote and every child it forked, so no
        // keeper or app child pid survives serve exit.
        self.zygote.shutdown_all();
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
                // FB5: the launcher activity and the guest's own pid, so the
                // owning MCP server can list its sessions with `activity` and a
                // guest RSS read from `/proc/<pid>/status`.
                "activity": session.activity(),
                "guest_pid": session.guest_pid(),
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
                // SERVELEAK2/F3: which launch owns the id right now, so a client
                // can check that the session it is about to destroy is still the
                // guest *it* launched.
                "session_token": held.launch_token(),
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
                            &mut session,
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
        match ArtSession::start_with_zygote(
            &apk,
            &artifacts,
            animations,
            insets,
            Some(Arc::clone(&self.zygote)),
        ) {
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
                    launch_token: Mutex::new(String::new()),
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
                let mut guard = held.session.lock().unwrap_or_else(|e| e.into_inner());
                launch_reply(
                    request_id,
                    id,
                    &held,
                    &mut guard,
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
            // SERVELEAK: a destroy that names a leftover must kill it. The
            // teardown proves every process group the session owns (launcher and
            // guest); this is the wider session check (a launcher left behind
            // outside those groups) and it *acts*: it SIGKILLs what it finds and
            // reports only what is still alive afterwards.
            let leftovers = session.reap_leftovers();
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
                            "session_leftovers_reaped": true,
                        }),
                    )
                }
                // The cleanup above still ran: the reply reports the failure
                // **after** the tree was killed and proven, never instead.
                Err(error) => (
                    false,
                    json!({
                        "error": error.to_protocol_error(),
                        "lifecycle": path,
                        "teardown_ms": teardown_ms,
                        "session_leftovers": leftovers,
                        "session_leftovers_reaped": true,
                    }),
                ),
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
            "acos_observe_tree" => observe_tree(&mut session, &held.artifacts, args),
            "acos_observe" => observe(&mut session, &held.artifacts, args),
            "acos_tap" => tap(&mut session, args),
            "acos_text" => type_text(&mut session, args),
            "acos_key" => key(&mut session, args),
            "acos_assert" => assert(&mut session, args, trust),
            "acos_logs" => logs(&mut session, args),
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
    session: &mut ArtSession,
    tree: &UiTree,
    fresh_guest: bool,
    replaced_guest: Option<Value>,
    started: std::time::Instant,
) -> Envelope {
    let (max_nodes, node_count, truncated, tree_value) = tree_json(tree, 2000);
    // SERVELEAK2/F3: this launch now owns the id. A client that launched the
    // session earlier keeps its old token and can see that its guest was
    // replaced (see `SessionBackend::shutdown`).
    let session_token = held.renew_launch_token();
    let mut result = json!({
        "session_id": id,
        "session_token": session_token,
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
        // FB5: `ms` is the time from launch start to when this rung's evidence
        // was **observed** — not a guess. `acos-art`'s ready line proves
        // `process`, `activity_resumed` and `first_frame` in one observation,
        // so the three carry the measured boot time; rungs a future launcher
        // reports earlier would carry their own. `ms_basis` says which.
        "boot_ms": session.session_start_ms(),
        "ms_basis": "observed_at_ready",
        "stages": [
            {"stage": "process", "ok": true, "status": "ok", "ms": session.session_start_ms()},
            {"stage": "activity_resumed", "ok": true, "status": "ok", "ms": session.session_start_ms()},
            {"stage": "first_frame", "ok": true, "status": "ok", "ms": session.session_start_ms()}
        ],
    });
    label_meta(&mut result, ENGINE_ART, held.trust, EVIDENCE_HOST_PIXEL);
    // ZPROD3: a session that really runs a zygote child says so, with the facts
    // the keeper acked. With the flag off `merge_zygote_fields` changes nothing,
    // so the launch reply is unchanged byte for byte.
    merge_zygote_fields(&mut result, session);
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
        if report.placeholder || report.suspect {
            if let Some(frame) = result.get_mut("frame").and_then(Value::as_object_mut) {
                frame.insert("meaningful".into(), json!(!report.placeholder));
                frame.insert("placeholder".into(), json!(report.placeholder));
                if report.suspect {
                    // FB2c: a text-free skeleton whose frame moved is a normal
                    // frame with a warning, never a blocked verdict.
                    frame.insert("placeholder_suspect".into(), json!(true));
                }
            }
            if report.suspect {
                result["warnings"] = json!([{
                    "code": "PLACEHOLDER_SUSPECT",
                    "message": "the first frame is the window skeleton (no app text), but its pixels \
                                changed and/or a newer frame was published: reported as a normal frame \
                                (a text-free custom View / SurfaceView / GL / image-only screen looks \
                                like this), not as a placeholder. If the screen should show app UI, \
                                check that the content actually mounted.",
                    "frame_seq": report.frame_seq,
                    "pixels_changed": report.pixels_changed,
                    "frames_advanced": report.frames_advanced,
                }]);
            }
        }
    }
    label_timing(&mut result, session, "acos_launch", started);
    // FB2: a window whose content never mounted is **not** a launch pass. The
    // placeholder label above was the evidence `acos_launch` used to publish as
    // `pass` / `first_frame` (feedback: launch `pass`, `first_frame`, 9 nodes,
    // `pixels: false`, stuck for 40+ s while SoLoader looped on a missing
    // `libworklets.so`). Report it as `blocked` with `reason:
    // placeholder-content` and the first log line that explains it.
    // FB2c: the label is not sticky — `placeholder_frame_report()` answers only
    // while the *current* bound frame is still the placeholder.
    let placeholder_failure = match session.placeholder_frame_report().cloned() {
        Some(report) => {
            let lines = log_lines_for_hint(session);
            Some(placeholder_launch_error(
                id,
                &report,
                session.last_frame_number(),
                placeholder_hint_line(&lines),
            ))
        }
        None => None,
    };
    if let Some(error) = placeholder_failure {
        // The ladder is published with the reply, so a caller sees *where* the
        // launch stopped even though the envelope is a failure.
        result["stage_reached"] = json!("window");
        result["content"] = json!({
            "mounted": false,
            "reason": "placeholder-content",
            "note": "the window and its decor exist; the app's own content never mounted",
        });
        if let Some(stages) = result.get_mut("stages").and_then(Value::as_array_mut) {
            for stage in stages.iter_mut() {
                if stage.get("stage").and_then(Value::as_str) == Some("first_frame") {
                    *stage = json!({
                        "stage": "first_frame",
                        "ok": false,
                        "status": "blocked",
                        "blocker": {
                            "code": ErrorCode::PlaceholderContent.as_str(),
                            "message": error.message,
                        },
                    });
                }
            }
        }
        return Envelope::failure(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::ArtReference,
            error,
            Metrics::wall_ms(0),
        )
        .with_session_id(id.to_string())
        .with_value_result(result)
        .with_capabilities(["apk.inspect", "lifecycle", "ui.frame", "ui.tree"]);
    }
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

/// FB2: the guest log lines a launch reply may quote.
///
/// The sink drops oldest first and a React Native launch opens with hundreds of
/// SoLoader lines, so the launch reply carries at most one *chosen* line rather
/// than a wall of them; `acos_logs` remains the whole record.
fn log_lines_for_hint(session: &mut ArtSession) -> Vec<String> {
    let priority = |level: LogLevel| match level {
        LogLevel::Verbose => "V",
        LogLevel::Debug => "D",
        LogLevel::Info => "I",
        LogLevel::Warn => "W",
        LogLevel::Error => "E",
    };
    match session.logs_since(0) {
        Ok(records) => records
            .iter()
            .map(|record| {
                format!(
                    "{} {:<8} {}",
                    priority(record.level),
                    record.tag,
                    record.message
                )
            })
            .collect(),
        // A launch whose guest is wedged cannot promise a log snapshot; the
        // error still says `placeholder-content` and points at `acos_logs`.
        Err(_) => Vec::new(),
    }
}

/// FB2: the first line that *explains* a launch whose content never mounted.
///
/// Ordered by how much it tells an agent: a missing native symbol names the
/// library, SoLoader's "couldn't find" line names it too, then a fatal JS/
/// runtime line, then any error-level line. Never a guess: when no line matches,
/// the reply says so instead of quoting noise.
fn placeholder_hint_line(lines: &[String]) -> Option<String> {
    const MARKERS: [&str; 5] = [
        "UnsatisfiedLinkError",
        "couldn't find",
        "SoLoader",
        "FATAL EXCEPTION",
        "FATAL",
    ];
    for marker in MARKERS {
        if let Some(line) = lines.iter().find(|line| line.contains(marker)) {
            return Some(line.trim().to_string());
        }
    }
    lines
        .iter()
        .find(|line| line.starts_with('E'))
        .map(|line| line.trim().to_string())
}

/// FB2: the structured failure for a launch that reached only the window.
///
/// `blocked` / `PLACEHOLDER_CONTENT` (reason `placeholder-content`), with the
/// bound-skeleton facts and one log line. Never a pass: the whole point is that
/// `pass` + `first_frame` on a splash that never mounts is a false green.
fn placeholder_launch_error(
    id: &str,
    report: &crate::engine::art::FirstFrameReport,
    frame_number: Option<u64>,
    log_hint: Option<String>,
) -> ProtocolError {
    let sentence = crate::engine::art::placeholder_evidence_sentence(report);
    let detail = format!(
        "launch reached the window skeleton and nothing about the frame moved, so the app's content \
         never mounted ({} node(s), no app text) after {} traversal(s)/{} ms ({} event wait(s)); \
         {sentence}; reason: placeholder-content",
        report.nodes, report.traversals, report.waited_ms, report.event_waits
    );
    let mut details = json!({
        "reason": "placeholder-content",
        "session_id": id,
        "nodes": report.nodes,
        "traversals": report.traversals,
        "event_waits": report.event_waits,
        "waited_ms": report.waited_ms,
        "settled": report.settled,
        "bound": report.bound,
        "frame_number": frame_number,
        "frame_number_advanced": report.frames_advanced,
        "pixels_compared": report.pixels_compared,
        "pixels_changed": report.pixels_changed,
        "budget_ms": report.budget_ms,
    });
    let hint = match &log_hint {
        Some(line) => {
            details["log_hint"] = json!(line);
            format!(
                "The window exists; the app's own content never mounted. The launch already waited its \
                 whole content budget (`ACOS_ART_FIRST_FRAME_MS`, {} ms), so a longer timeout cannot \
                 mount content that is not there: the guest log names the cause in this line: \
                 `{line}`. A native library the app references but the APK does not ship for the host \
                 ABI is the usual cause: run `acos_inspect` and read its `warnings`, then `acos_logs` \
                 around that line.",
                report.budget_ms
            )
        }
        None => format!(
            "The window exists; the app's own content never mounted and the guest log carried no \
             explaining line. The launch already waited its whole content budget \
             (`ACOS_ART_FIRST_FRAME_MS`, {} ms); a longer timeout only reproduces this. Read \
             `acos_logs` for the first UnsatisfiedLinkError / SoLoader / fatal JS line; \
             `acos_inspect` cross-checks the APK's referenced native libraries against lib/<abi>/.",
            report.budget_ms
        ),
    };
    ProtocolError::new(ErrorCode::PlaceholderContent, detail)
        .with_hint(hint)
        .with_details(details)
}

/// FB2: what a caller asked for with `screenshot`.
///
/// `true` means "write it where this session keeps its frames and tell me the
/// path" (the agent-facing form: no path to invent); a string keeps the CLI's
/// `--screenshot <path>` contract. Anything else is an argument error, never a
/// silent no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ScreenshotRequest {
    None,
    Default,
    Path(PathBuf),
}

fn screenshot_request(args: &Value) -> Result<ScreenshotRequest, BackendError> {
    match args.get("screenshot") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(ScreenshotRequest::None),
        Some(Value::Bool(true)) => Ok(ScreenshotRequest::Default),
        Some(Value::String(path)) if !path.trim().is_empty() => {
            Ok(ScreenshotRequest::Path(PathBuf::from(path)))
        }
        Some(other) => Err(BackendError::Error {
            code: ErrorCode::InvalidArgument,
            detail: format!(
                "`screenshot` must be true (write under the session's artifacts and return the \
                 path) or a file path (got {other})"
            ),
        }),
    }
}

/// The pixel size in a PNG's IHDR chunk (16-byte signature + 8-byte length/type
/// header, then width and height). Header-only: an 80 KB frame is never decoded
/// just to say how big it is.
fn png_pixel_size(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read as _;
    let mut header = [0u8; 24];
    let mut file = std::fs::File::open(path).ok()?;
    file.read_exact(&mut header).ok()?;
    if &header[..8] != b"\x89PNG\r\n\x1a\n" || &header[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
    let height = u32::from_be_bytes([header[20], header[21], header[22], header[23]]);
    Some((width, height))
}

/// FB2: write the session's current frame as a PNG and describe it.
///
/// The bytes are the launcher's own artifact — already a compressed lossless
/// PNG (SHOTZ), so this is one kernel-side copy and never a re-encode. The reply
/// carries the absolute path, the pixel size, the display size and
/// `display_origin` (so a screenshot coordinate converts to a tree coordinate),
/// and the frame identity it is bound to.
///
/// A frame with no app pixels (the launch placeholder, or a guest that published
/// nothing) is a structured `NO_FRESH_FRAME`, never an empty success and never
/// a file the caller mistakes for the app.
fn attach_screenshot(
    session: &mut ArtSession,
    artifacts: &Path,
    request: ScreenshotRequest,
    verb: &str,
    result: &mut Value,
) -> Result<(), BackendError> {
    let request_dest = match request {
        ScreenshotRequest::None => return Ok(()),
        ScreenshotRequest::Default => None,
        ScreenshotRequest::Path(path) => Some(path),
    };
    if session.placeholder_frame() {
        return Err(placeholder_screenshot_error(verb));
    }
    let (source, frame_seq, md5, bytes) = session.screenshot_source()?;
    write_screenshot(
        artifacts,
        request_dest,
        verb,
        (&source, frame_seq, &md5, bytes),
        (session.width(), session.height()),
        session.display_origin(),
        result,
    )
}

/// FB2: a screenshot requested on the launch placeholder.
///
/// Structured `NO_FRESH_FRAME` — not an empty success, not a silent no-op, and
/// not splash pixels presented as the app: the window skeleton has no app
/// content to photograph.
fn placeholder_screenshot_error(verb: &str) -> BackendError {
    super::live_frame_unavailable(format!(
        "the current frame is the launch placeholder (the window skeleton: the app's content \
         never mounted), so it has no screenshot to give; this is a placeholder/no-pixel frame, \
         not a fresh app frame (reason: placeholder-content). Read `first_frame`/`acos_logs` for \
         why the content did not mount ({verb})"
    ))
}

/// FB2c: confine a screenshot destination to the session's artifacts directory.
///
/// The argument came from an MCP caller, so it is untrusted input: `true` means
/// the session's own `screenshots/` dir, a relative path resolves **against that
/// directory** (never the daemon's cwd), and an absolute path must stay inside
/// it. `..` components, a symlink leaving the directory and a destination that
/// is the source frame itself are all refused before anything is created or
/// copied — the old code called `create_dir_all` on any path and could truncate
/// the frame artifact to zero bytes.
fn confine_screenshot_dest(
    artifacts: &Path,
    request_dest: Option<PathBuf>,
    verb: &str,
    frame_seq: u64,
    source: &Path,
) -> Result<PathBuf, BackendError> {
    let base = std::fs::canonicalize(artifacts).map_err(|error| BackendError::Error {
        code: ErrorCode::RenderIo,
        detail: format!(
            "the session artifacts directory {} is not usable: {error}",
            artifacts.display()
        ),
    })?;
    let Some(requested) = request_dest else {
        let dir = base.join("screenshots");
        std::fs::create_dir_all(&dir).map_err(|error| BackendError::Error {
            code: ErrorCode::RenderIo,
            detail: format!("cannot create {}: {error}", dir.display()),
        })?;
        return Ok(dir.join(format!("{}-{frame_seq}.png", verb.replace("acos_", ""))));
    };
    let reject = |why: &str| BackendError::Error {
        code: ErrorCode::InvalidArgument,
        detail: format!(
            "`screenshot` must be true or a path inside the session's artifacts directory ({}); \
             {why}",
            base.display()
        ),
    };
    if requested
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(reject("a `..` component is refused"));
    }
    if requested.as_os_str().is_empty() {
        return Err(reject("an empty path is refused"));
    }
    let candidate = if requested.is_absolute() {
        requested.clone()
    } else {
        base.join(&requested)
    };
    let Some(name) = candidate.file_name().map(|n| n.to_os_string()) else {
        return Err(reject("the path names no file"));
    };
    let Some(parent) = candidate.parent() else {
        return Err(reject("the path names no directory"));
    };
    // Refuse *before* creating anything: resolve the deepest ancestor that
    // already exists, so a symlink that leaves the artifacts directory is caught
    // without mkdir'ing through it.
    let mut probe = parent.to_path_buf();
    while !probe.exists() {
        let Some(up) = probe.parent().map(Path::to_path_buf) else {
            return Err(reject("no existing ancestor directory"));
        };
        probe = up;
    }
    let probe_real = std::fs::canonicalize(&probe).map_err(|error| BackendError::Error {
        code: ErrorCode::RenderIo,
        detail: format!("cannot resolve {}: {error}", probe.display()),
    })?;
    if !probe_real.starts_with(&base) {
        return Err(reject(&format!(
            "{} is outside the artifacts directory",
            probe_real.display()
        )));
    }
    std::fs::create_dir_all(parent).map_err(|error| BackendError::Error {
        code: ErrorCode::RenderIo,
        detail: format!("cannot create {}: {error}", parent.display()),
    })?;
    // Canonicalize *after* creating the directory: symlinks resolve, so a link
    // that leaves the artifacts directory is caught here, not by the write.
    let parent_real = std::fs::canonicalize(parent).map_err(|error| BackendError::Error {
        code: ErrorCode::RenderIo,
        detail: format!("cannot resolve {}: {error}", parent.display()),
    })?;
    if !parent_real.starts_with(&base) {
        return Err(reject(&format!(
            "{} is outside the artifacts directory",
            parent_real.display()
        )));
    }
    let dest = parent_real.join(name);
    // A symlink as the last component would be followed by `fs::copy` (writing
    // through it, wherever it points).
    if std::fs::symlink_metadata(&dest).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(reject(&format!("{} is a symlink", dest.display())));
    }
    let source_real = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let dest_real = std::fs::canonicalize(&dest).unwrap_or_else(|_| dest.clone());
    if dest_real == source_real {
        return Err(reject(
            "the destination is the frame artifact itself; copying it would truncate the frame",
        ));
    }
    Ok(dest)
}

/// The write/report half of [`attach_screenshot`], for a caller that already
/// holds the frame's artifact (a verb that just captured one).
fn write_screenshot(
    artifacts: &Path,
    request_dest: Option<PathBuf>,
    verb: &str,
    (source, frame_seq, md5, bytes): (&Path, u64, &str, u64),
    (display_width, display_height): (u32, u32),
    (origin_x, origin_y): (i32, i32),
    result: &mut Value,
) -> Result<(), BackendError> {
    if bytes == 0 {
        return Err(super::live_frame_unavailable(
            "the frame artifact for this screenshot is empty",
        ));
    }
    let dest = confine_screenshot_dest(artifacts, request_dest, verb, frame_seq, source)?;
    std::fs::copy(source, &dest).map_err(|error| BackendError::Error {
        code: ErrorCode::RenderIo,
        detail: format!(
            "cannot write the screenshot {} from {}: {error}",
            dest.display(),
            source.display()
        ),
    })?;
    let absolute = std::fs::canonicalize(&dest).unwrap_or_else(|_| dest.clone());
    let (width, height) = png_pixel_size(&absolute).ok_or_else(|| BackendError::Error {
        code: ErrorCode::RenderIo,
        detail: format!(
            "the screenshot {} is not a readable PNG; refusing to describe it",
            absolute.display()
        ),
    })?;
    result["screenshot"] = json!({
        "path": absolute.display().to_string(),
        "bytes": bytes,
        "format": "png",
        "frame_seq": frame_seq,
        "md5": md5,
        "width": width,
        "height": height,
        "display_size": [display_width, display_height],
        "display_origin": [origin_x, origin_y],
        "note": "lossless PNG of the frame above; png_x = display_x - display_origin.x, png_y = display_y - display_origin.y (tree bounds are display space)",
    });
    Ok(())
}

fn observe_tree(
    session: &mut ArtSession,
    artifacts: &Path,
    args: &Value,
) -> Result<Value, BackendError> {
    // FB2: `screenshot: true` is an argument error before any work is done.
    let screenshot = screenshot_request(args)?;
    let max_nodes = args
        .get("max_nodes")
        .and_then(Value::as_u64)
        .unwrap_or(2000) as usize;
    // P3b: the tree verb consumes P1's settle vocabulary. `strict` is the
    // default and unchanged; `fast` is for inspection/navigation, labelled in
    // the reply, and never accepted by an assertion.
    let settle = requested_settle(args)?;
    let t_begin = m5_now_us();
    // FB2c item 6: a screenshot is captured by the same traversal as the tree,
    // so the two cannot disagree about which frame is on screen.
    let tree = session
        .tree_with_settle_pixels(settle, !matches!(screenshot, ScreenshotRequest::None))?;
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
    let mut result = json!({
        "package": session.package(),
        "stage_reached": "first_frame",
        "windows": distinct_windows(&tree),
        "truncated": truncated,
        "max_nodes": max_nodes,
        "node_count": node_count,
        "tree": tree_value,
        "settle": session.last_tree_settle().cloned(),
    });
    // FB2: the screenshot is of *this* bound frame, and its `display_origin`
    // says how a PNG pixel maps to a tree bound.
    attach_screenshot(session, artifacts, screenshot, "acos_observe_tree", &mut result)?;
    Ok(result)
}

fn observe(
    session: &mut ArtSession,
    artifacts: &Path,
    args: &Value,
) -> Result<Value, BackendError> {
    let want_tree = args.get("tree").and_then(Value::as_bool).unwrap_or(false);
    let want_text = args.get("text").and_then(Value::as_bool).unwrap_or(false);
    let settle = requested_settle(args)?;
    let screenshot = screenshot_request(args)?;
    let path = match &screenshot {
        ScreenshotRequest::Path(path) => Some(path.clone()),
        _ => None,
    };
    if want_tree || want_text {
        let tree = session
            .tree_with_settle_pixels(settle, !matches!(screenshot, ScreenshotRequest::None))?;
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
        // FB2: `screenshot` used to be silently ignored on the tree path (the
        // agent got a tree, no file, no error). It is honoured here on the same
        // bound frame.
        attach_screenshot(session, artifacts, screenshot, "acos_observe", &mut result)?;
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
    // FB2: the capture above *is* the frame this verb answers with, so the
    // screenshot is written from it (never a second capture) and reported with
    // its `display_origin`. No `screenshot` argument keeps the previous shape:
    // the capture is reported, no copy is made.
    if screenshot == ScreenshotRequest::None {
        return Ok(result);
    }
    let t_write = m5_now_us();
    write_screenshot(
        artifacts,
        path,
        "acos_observe",
        (&frame.path, frame.frame_seq, &frame.md5, frame.bytes),
        (session.width(), session.height()),
        session.display_origin(),
        &mut result,
    )?;
    m5_trace(
        "cli.serve_shot_write",
        t_write,
        m5_now_us(),
        &format!("png_bytes={}", frame.bytes),
    );
    Ok(result)
}

fn tap(session: &mut ArtSession, args: &Value) -> Result<Value, BackendError> {
    let settle = requested_settle(args)?;
    // FB3: the reply describes what the action changed (default on).
    session.set_diff_requested(diff_requested(args)?);
    // SE1: reject an unverifiable pass before the input is injected.
    reject_expect_change_without_pixel_evidence(args)?;
    let result = if let Some((selector, describe)) = args_selector(args) {
        // Input resolution, not evidence resolution: a `text` selector reaches
        // the platform's accessible name (text, else content description among
        // nodes the user can see) -- see `UiTree::find_input_target`. An
        // explicit `content_desc` selector names that field exactly instead.
        // Either way several user-visible matches are refused, never silently
        // picked.
        let tree = session.ensure_bound_tree()?;
        let target = resolve_input_node_in(&tree, &selector, &describe)?;
        session.set_settle_mode(settle);
        session.begin_input();
        let (x, y) = target.tap_point();
        session.tap_in_window(&target.window, x, y)?;
        // REL3E: the hit-test uses the tree the selector resolved against, i.e.
        // the same geometry `tap_point` came from; no extra traversal.
        let target = tap_target(Some(&describe), Some(&target), Some(&tree), x, y);
        input_result(session, "tap", target)
    } else {
        let (Some(x), Some(y)) = (
            args.get("x").and_then(Value::as_i64),
            args.get("y").and_then(Value::as_i64),
        ) else {
            return Err(BackendError::Failed {
                code: ErrorCode::InvalidArgument,
                detail: "acos_tap needs `id`, `text`, `content_desc`, or both `x` and `y`"
                    .to_string(),
            });
        };
        // A coordinate tap has no selector; its hit-test still comes from the
        // already-bound tree (never a new traversal).
        let tree = session.bound_tree();
        session.set_settle_mode(settle);
        session.begin_input();
        session.tap(x as i32, y as i32)?;
        let target = tap_target(None, None, tree.as_ref(), x as i32, y as i32);
        input_result(session, "tap", target)
    };
    check_expect_change(args, session, &result)?;
    Ok(result)
}

/// The selector an input/assert argument names, with the JSON that describes it.
///
/// Precedence is `id` > `content_desc` > `text` (an id is the narrowest thing an
/// agent can name, a content description the platform's other *explicit* field,
/// and `text` the accessible-name convenience). `index`, when given, is ANDed
/// with whichever field was used, so `{"text": "Continue", "index": 1}` is the
/// escape hatch for a legitimately repeated label.
fn args_selector(args: &Value) -> Option<(Selector, Value)> {
    let index = args.get("index").and_then(Value::as_u64).map(|i| i as usize);
    let mut selector = if let Some(id) = args.get("id").and_then(Value::as_str) {
        Selector::by_id(id)
    } else if let Some(desc) = args.get("content_desc").and_then(Value::as_str) {
        Selector {
            content_desc: Some(acos_protocol::ui::TextMatch::exact(desc)),
            ..Selector::default()
        }
    } else {
        let text = args.get("text").and_then(Value::as_str)?;
        Selector::by_text(text)
    };
    selector.index = index;
    let mut describe = json!({});
    let map = describe.as_object_mut().expect("object");
    if let Some(id) = selector.id.clone() {
        map.insert("id".into(), json!(id));
    }
    if let Some(text) = selector.text.as_ref().and_then(|t| t.exact.clone()) {
        map.insert("text".into(), json!(text));
    }
    if let Some(desc) = selector.content_desc.as_ref().and_then(|t| t.exact.clone()) {
        map.insert("content_desc".into(), json!(desc));
    }
    if let Some(index) = index {
        map.insert("index".into(), json!(index));
    }
    Some((selector, describe))
}

/// Resolve a selector to exactly one node the user can see.
///
/// One match is the answer. Several are **refused** with `AMBIGUOUS_SELECTOR` and
/// the full candidate list, because a silent first match is how an agent taps
/// the wrong control (two React Native tabs can share a label); the caller can
/// then name one with `id`/`content_desc`/`bounds` or `index`. No match keeps the
/// established ladder: a node that exists but is not visible is
/// `ELEMENT_OFFSCREEN` (with its bounds), a complete tree with nothing is
/// `ELEMENT_NOT_FOUND`, a truncated tree is `TREE_TRUNCATED`.
fn resolve_input_node_in(
    tree: &UiTree,
    selector: &Selector,
    describe: &Value,
) -> Result<UiNode, BackendError> {
    let candidates = tree.find_input_candidates(selector);
    match candidates.len() {
        1 => return Ok(candidates[0].clone()),
        0 => {}
        _ => return Err(ambiguous_selector(describe, &candidates)),
    }
    if let Some(offscreen) = tree.find_invisible(selector) {
        return Err(BackendError::Failed {
            code: ErrorCode::ElementOffscreen,
            detail: format!(
                "{describe} matches only a node that is not visible to the user (text {:?}, bounds {}, visible_bounds {}); scroll it into view first",
                offscreen.text,
                offscreen.bounds,
                offscreen
                    .visible_bounds
                    .map_or_else(|| "<none>".to_string(), |b| b.to_string())
            ),
        });
    }
    Err(BackendError::Failed {
        code: if tree.truncated {
            ErrorCode::TreeTruncated
        } else {
            ErrorCode::ElementNotFound
        },
        detail: format!(
            "no node matching {describe} in the live ART tree{}",
            if tree.truncated {
                " (the tree was truncated: absence is not proven)"
            } else {
                ""
            }
        ),
    })
}

/// The structured refuse for a selector that names more than one visible node.
///
/// `BackendError::Protocol` keeps the shape: `error.details.candidates` is the
/// machine-readable list (`id`, `resource_id`, `text`, `content_desc`, `class`,
/// `bounds`), so an agent can pick one without re-observing and without ever
/// having to parse a message.
fn ambiguous_selector(describe: &Value, candidates: &[&UiNode]) -> BackendError {
    let listed: Vec<Value> = candidates
        .iter()
        .map(|node| {
            json!({
                "id": node.id,
                "resource_id": node.resource_id,
                "text": node.text,
                "content_desc": node.content_desc,
                "class": node.class,
                "bounds": node.bounds.to_string(),
                "visible": node.visible,
                "clickable": node.clickable,
            })
        })
        .collect();
    BackendError::Protocol {
        code: ErrorCode::AmbiguousSelector,
        detail: format!(
            "{describe} matches {} nodes the user can see; nothing was tapped or asserted. \
             Name one with `id`, `content_desc`, `bounds`, or `index` (candidates: {})",
            candidates.len(),
            listed
                .iter()
                .map(|c| format!(
                    "{} text={:?} content_desc={:?} {}",
                    c["id"], c["text"], c["content_desc"], c["bounds"]
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ),
        hint: Some(
            "A silent first match is how an agent acts on the wrong control. Pass `id`, \
             `content_desc` or `index`, or narrow with `content_desc`; the candidates are in \
             `error.details.candidates`."
                .to_string(),
        ),
        details: Some(json!({
            "selector": describe,
            "count": candidates.len(),
            "candidates": listed,
        })),
    }
}

/// The node the typed text lands in, by the tree's own evidence: the visible node
/// that reports `focused`. `None` means the tree names no focused field, so the
/// target is unknown. No other node is ever substituted: a screen can show an
/// unfocused credential field next to the one being typed into, and reporting
/// it as the target would be wrong evidence.
fn focused_input_node(tree: &UiTree) -> Option<&UiNode> {
    tree.all_nodes()
        .into_iter()
        .find(|node| node.visible && node.focused)
}

/// The reply's echo decision for `acos_text`: the node the input went to (as the
/// tree shows it) and, when the typed characters must be withheld, why.
struct EchoDecision<'a> {
    node: Option<&'a UiNode>,
    /// `None` = the typed text may be echoed. `Some(reason)` = it is masked.
    mask_reason: Option<&'static str>,
}

/// Mask unless the typed text is known to land in a non-password field:
/// - `mask: true` always masks (the caller knows it is a secret);
/// - a focused `password` node masks;
/// - no focused node at all (target unknown) masks, never a guess.
/// Only a focused, non-password node is echoed.
fn echo_decision(tree: &UiTree, forced: bool) -> EchoDecision<'_> {
    let node = focused_input_node(tree);
    let mask_reason = if forced {
        Some("requested with mask: true")
    } else {
        match node {
            Some(focused) if focused.password => Some(
                "password field (the platform flags the node and still reports its text; ACOS never echoes typed characters)",
            ),
            Some(_) => None,
            None => Some(
                "target unknown: the tree names no focused field, so the typed text is withheld",
            ),
        }
    };
    EchoDecision { node, mask_reason }
}

/// The node as the reply describes it: identity, its accessible name, its
/// geometry and the input flags an agent has to reason about.
///
/// A password node's own text is withheld too: it is the secret the field holds,
/// not the agent's typing.
fn node_summary(node: &UiNode, withhold_text: bool) -> Value {
    json!({
        "id": node.id,
        "class": node.class,
        "resource_id": node.resource_id,
        "content_desc": node.content_desc,
        "text": if withhold_text { Value::Null } else { json!(node.text) },
        "bounds": node.bounds.to_string(),
        "visible_bounds": node.visible_bounds.map(|b| b.to_string()),
        "clickable": node.clickable,
        "scrollable": node.scrollable,
        "enabled": node.enabled,
        "focused": node.focused,
        "password": node.password,
        "window": node.window,
    })
}

/// REL3E: the `target` block of a tap reply.
///
/// It answers the two questions a `state_changed: false` left open: **what the
/// selector resolved to** and **what the injected point actually hit**. `hit` is
/// the tree-geometry answer, computed from the same tree the tap resolved
/// against; whether the app consumed the press is `handled` in the reply, which
/// comes from the guest's `finishInputEvent(handled)` and never from this
/// geometry.
fn tap_target(
    selector: Option<&Value>,
    node: Option<&UiNode>,
    tree: Option<&UiTree>,
    x: i32,
    y: i32,
) -> Value {
    let mut target = serde_json::Map::new();
    if let Some(selector) = selector {
        target.insert("selector".into(), selector.clone());
    }
    if let Some(node) = node {
        target.insert("node".into(), node_summary(node, false));
    }
    target.insert("point".into(), json!({"x": x, "y": y}));
    let hit = tree.map(|tree| tree.tap_hit(x, y));
    let mut hit_json = serde_json::Map::new();
    hit_json.insert("method".into(), json!("tree-geometry"));
    match &hit {
        Some(hit) => {
            hit_json.insert(
                "node".into(),
                hit.node().map_or(Value::Null, |node| node_summary(node, false)),
            );
            hit_json.insert("clickable".into(), json!(hit.node().is_some_and(|n| n.clickable)));
            hit_json.insert(
                "consumer".into(),
                hit.consumer.map_or(Value::Null, |node| node_summary(node, false)),
            );
            hit_json.insert(
                "suggest".into(),
                hit.suggestion
                    .map_or(Value::Null, |node| node_summary(node, false)),
            );
        }
        None => {
            // No tree: the point was never hit-tested. Say so rather than
            // pretending nothing is there.
            hit_json.insert("node".into(), Value::Null);
            hit_json.insert("clickable".into(), Value::Null);
            hit_json.insert("consumer".into(), Value::Null);
            hit_json.insert("suggest".into(), Value::Null);
            hit_json.insert("note".into(), json!("no bound tree to hit-test against"));
        }
    }
    target.insert("hit".into(), Value::Object(hit_json));
    Value::Object(target)
}

/// The `name` an agent would use for a node: its text, else its content
/// description (the platform's accessible name), trimmed for a one-line hint.
fn node_name(node: &Value) -> String {
    for field in ["text", "content_desc"] {
        if let Some(value) = node.get(field).and_then(Value::as_str) {
            if !value.trim().is_empty() {
                return format!("{value:?}");
            }
        }
    }
    String::from("(no name)")
}

/// REL3E: the one-line reason a reply is not enough on its own.
///
/// It only ever explains the framework fact (`handled`) together with the
/// hit-test result; it never invents a reason the evidence does not show.
fn handling_hint(target: Option<&Value>, handled: Option<bool>, state_changed: bool) -> Option<String> {
    let hit = target.and_then(|target| target.get("hit"));
    let consumer = hit.and_then(|hit| hit.get("consumer")).filter(|v| !v.is_null());
    let suggest = hit.and_then(|hit| hit.get("suggest")).filter(|v| !v.is_null());
    let hit_node = hit.and_then(|hit| hit.get("node")).filter(|v| !v.is_null());
    match handled {
        None => Some(
            "the guest did not ack every event of this action (see handled_evidence.published \
             vs acked), so whether the app consumed it is unknown; nothing here claims it was \
             handled or that nothing happened"
                .to_string(),
        ),
        Some(false) => {
            if let Some(consumer) = consumer {
                return Some(format!(
                    "the app's window did NOT consume this input even though the point is on the \
                     clickable {} {} at {} (its own `finishInputEvent(handled)` answered false): \
                     the control is disabled or a parent intercepted the press, so no state change \
                     is expected",
                    consumer["class"].as_str().unwrap_or("view"),
                    node_name(consumer),
                    consumer["bounds"].as_str().unwrap_or("?"),
                ));
            }
            let Some(hit_node) = hit_node else {
                return Some(
                    "nothing at this point consumed the input and the tree has no node there \
                     (the app's window answered `finishInputEvent(handled) = false`); nothing \
                     changed because nothing was tappable"
                        .to_string(),
                );
            };
            let mut hint = format!(
                "nothing tappable there: the node at this point is {} {} at {}, which is not \
                 clickable and neither is any ancestor, and the app's window answered \
                 `finishInputEvent(handled) = false` (no handler consumed the input)",
                hit_node["class"].as_str().unwrap_or("view"),
                node_name(hit_node),
                hit_node["bounds"].as_str().unwrap_or("?"),
            );
            if let Some(suggest) = suggest {
                let (cx, cy) = suggest
                    .get("bounds")
                    .and_then(Value::as_str)
                    .and_then(|bounds| bounds.parse::<acos_protocol::ui::Bounds>().ok())
                    .map(|bounds| bounds.center())
                    .unwrap_or((0, 0));
                hint.push_str(&format!(
                    "; tap the nearest clickable {} {} at {} (centre {cx},{cy}) instead",
                    suggest["class"].as_str().unwrap_or("view"),
                    node_name(suggest),
                    suggest["bounds"].as_str().unwrap_or("?"),
                ));
            } else {
                hint.push_str(
                    "; this screen shows no clickable node, so scroll or open the control that \
                     owns the state first",
                );
            }
            Some(hint)
        }
        Some(true) if !state_changed => Some(
            "the app's window consumed the input (`finishInputEvent(handled) = true`) but the \
             settled frame shows no change: the handler may be asynchronous, or the control \
             already had that state. Assert on the state you expect instead of expecting a \
             change"
                .to_string(),
        ),
        Some(true) => None,
    }
}

/// `mask: true` on `acos_text`: the caller knows the field is a secret even when
/// the tree reports no focus (some React Native trees do not expose it).
fn mask_requested(args: &Value) -> Result<bool, BackendError> {
    match args.get("mask") {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(other) => Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: format!("`mask` must be a boolean (got {other})"),
        }),
    }
}

/// The reply's `target` for `acos_text`. The typed text is echoed only when the
/// decision allows it; otherwise the count is honest and the characters are not.
/// `node` is the field the input went to, or null when the target is unknown.
fn input_target(text: &str, decision: &EchoDecision<'_>) -> Value {
    match decision.mask_reason {
        Some(reason) => json!({
            "text": "*",
            "text_masked": true,
            "chars": text.chars().count(),
            "reason": reason,
            "node": decision.node.map(|node| node_summary(node, true)),
        }),
        None => json!({
            "text": text,
            "node": decision.node.map(|node| node_summary(node, false)),
        }),
    }
}

fn type_text(session: &mut ArtSession, args: &Value) -> Result<Value, BackendError> {
    let settle = requested_settle(args)?;
    session.set_diff_requested(diff_requested(args)?);
    reject_expect_change_without_pixel_evidence(args)?;
    let Some(text) = args.get("text").and_then(Value::as_str) else {
        return Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: "acos_text needs a string `text`".to_string(),
        });
    };
    // Typing is the *same* path the scenario `type` step takes: the guest's
    // `type_text` command drives the focused editor through the real input
    // connection (or its virtual-keyboard fallback), and the settle decides what
    // changed. The live channel used to refuse before trying whenever no node
    // ended in `EditText`, which is every React Native `TextInput` (obfuscated
    // class, placeholder as its text), so an agent could launch Gaten, focus the
    // search field and still be told `text.input` does not exist. Parity is the
    // fix: inject, then judge the settled result.
    //
    // The tree still matters for one thing: a password field must not echo the
    // typed characters anywhere.
    let tree = session.ensure_bound_tree()?;
    let decision = echo_decision(&tree, mask_requested(args)?);
    session.set_settle_mode(settle);
    session.begin_input();
    session.type_text(text)?;
    let target = input_target(text, &decision);
    let result = input_result(session, "text", target);
    check_expect_change(args, session, &result)?;
    Ok(result)
}

fn key(session: &mut ArtSession, args: &Value) -> Result<Value, BackendError> {
    let settle = requested_settle(args)?;
    session.set_diff_requested(diff_requested(args)?);
    reject_expect_change_without_pixel_evidence(args)?;
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
    // FB3: so does a top-level `diff`.
    if let (Some(diff), Some(object)) = (args.get("diff"), action.as_object_mut()) {
        object.entry("diff").or_insert_with(|| diff.clone());
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

/// SE2b: does `acos_assert` have to re-prove the content window first?
///
/// A content-settled bound frame is only evidence while it is the frame being
/// asserted. `bound_settle` records the *last* tree reply's rule, so in content
/// mode an assert must take a new content settle (fresh, distinct traversals)
/// instead of reading the cached tree. Strict/fast-bound sessions keep their
/// existing single-traversal behaviour.
fn assert_needs_fresh_content_settle(bound_settle: Option<&Value>) -> bool {
    bound_settle
        .and_then(|settle| settle.get("mode"))
        .and_then(Value::as_str)
        == Some("content")
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
    // SE2/SE2b: after a `settle: content` step the ONLY honest evidence is a
    // **new** content settle. The bound settle record is session-sticky (it
    // describes the last tree reply), so serving the cached tree here let an
    // `acos_assert` pass on a screen that had moved since: the assert must
    // re-prove the content window on fresh traversals, and never read the cache.
    let tree = if assert_needs_fresh_content_settle(session.bound_settle()) {
        session.content_settle()?;
        session.ensure_bound_tree()?
    } else {
        session.tree()?
    };
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
    let (selector, describe) = args_selector(args).ok_or_else(|| BackendError::Failed {
        code: ErrorCode::InvalidArgument,
        detail: "acos_assert needs `id`, `content_desc`, or `text` to select the node".to_string(),
    })?;
    let node = resolve_assert_node(&tree, &selector, &describe)?;
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

/// The **default** log view: `min_priority` `W` plus the app's own package.
///
/// Why: an agent that calls `acos_logs` to learn what the app said got ~235
/// lines of SoLoader/ViewManagerPropertyUpdater noise before the first
/// app-specific record, and paid for all of it in context. The default now
/// keeps records at warning or above **and** anything whose tag or message
/// names the launched package — a two-line view of the same run.
///
/// Nothing is lost: the guest buffer is untouched (and `logs/logcat.txt` in a
/// verify run is still the full log), `"all": true` or `min_priority: "V"`
/// returns every record, and an explicit `tag` narrows further.
fn logs(session: &mut ArtSession, args: &Value) -> Result<Value, BackendError> {
    let records = session.logs_since(0)?;
    let crash = session.last_crash();
    let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
    let requested = args.get("min_priority");
    let explicit_priority = match requested {
        None | Some(Value::Null) => None,
        Some(value) => {
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                other => other.to_string(),
            };
            match acos_runtime::session::parse_log_priority(&text) {
                Some(priority) => Some(priority),
                None => {
                    return Err(BackendError::Error {
                        code: ErrorCode::InvalidArgument,
                        detail: format!(
                            "min_priority `{text}` is not a logcat priority; use V/D/I/W/E/F or 2..7"
                        ),
                    })
                }
            }
        }
    };
    let tag = args
        .get("tag")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    let package = session.package().to_string();
    // An explicit request disables the default package union: the caller asked
    // for one filter and must get exactly that.
    let default_view = !all && explicit_priority.is_none() && tag.is_none();
    let floor = if all {
        2
    } else {
        explicit_priority.unwrap_or(5)
    };
    let package_filter = default_view && !package.is_empty();
    let total = records.len();
    let package = if package_filter {
        Some(package.as_str())
    } else {
        None
    };
    let records: Vec<_> = records
        .into_iter()
        .filter(|record| {
            keep_log_record(
                log_level_rank(record.level),
                &record.tag,
                &record.message,
                floor,
                tag.as_deref(),
                package,
            )
        })
        .collect();
    let filtered = total - records.len();
    let min_priority = match floor {
        2 => "V",
        3 => "D",
        4 => "I",
        5 => "W",
        6 => "E",
        _ => "F",
    };
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
        "total": total,
        "count": records.len(),
        "collapsed": 0,
        "filter": {
            "min_priority": min_priority,
            "package": if package_filter { json!(package) } else { Value::Null },
            "default_view": default_view,
            "dropped": filtered,
            "note": if default_view {
                "default view: priority >= W plus records naming the app package; pass \
                 \"all\": true (or min_priority \"V\") for the whole buffer"
            } else {
                "explicit filter applied; the guest buffer and logs/logcat.txt keep every record"
            },
        },
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

/// ZPROD3: copy the zygote fields into a reply (flag-on sessions only). With the
/// flag off the map is `None` and the reply object is untouched; with it on the
/// fields replace the generic `isolation` string, because a reset/status reply
/// must never describe a zygote child as an ordinary fresh process.
fn merge_zygote_fields(result: &mut Value, session: &ArtSession) {
    let Some(fields) = session.zygote_reply_fields() else {
        return;
    };
    let Some(object) = result.as_object_mut() else {
        return;
    };
    for (key, value) in fields {
        object.insert(key, value);
    }
}

/// logcat priority rank for a [`LogLevel`] (`2` = verbose … `6`/`7` = error).
fn log_level_rank(level: LogLevel) -> i32 {
    match level {
        LogLevel::Verbose => 2,
        LogLevel::Debug => 3,
        LogLevel::Info => 4,
        LogLevel::Warn => 5,
        LogLevel::Error => 6,
    }
}

/// One record's fate under the applied filter.
///
/// * an explicit `tag` substring is a hard AND filter (the caller asked for it);
/// * a record at or above `floor` is kept;
/// * otherwise a record is kept only when the default view is on **and** its tag
///   or message names the app's own package (so the app's `I`-level line about
///   itself survives the `W` floor while framework noise does not).
fn keep_log_record(
    rank: i32,
    tag: &str,
    message: &str,
    floor: i32,
    tag_filter: Option<&str>,
    package: Option<&str>,
) -> bool {
    if let Some(filter) = tag_filter {
        if !tag.to_ascii_lowercase().contains(filter) {
            return false;
        }
    }
    if rank >= floor {
        return true;
    }
    package.is_some_and(|package| tag.contains(package) || message.contains(package))
}

fn status_value(session: &ArtSession) -> Value {
    let mut status = json!({
        "launched": true,
        "package": session.package(),
        "activity": session.activity(),
        "guest_pid": session.guest_pid(),
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
    });
    merge_zygote_fields(&mut status, session);
    status
}

fn reset(session: &mut ArtSession, args: &Value) -> Result<Value, BackendError> {
    // P9: reset consumes the same settle vocabulary as launch. `strict` is the
    // default and unchanged (150 ms quiet window + capture); `fast` binds the
    // relaunched guest's first traversal from a fresh probe sample, with no
    // quiet window and no PNG.
    // SE2b: lo, `settle: content` on `acos_reset`. The reset binds the
    // relaunched guest's first frame with a strict/fast traversal; a content
    // rule there was silently coerced to strict, which is exactly the kind of
    // quiet downgrade the rule must never allow. Refuse it instead.
    let (settle, pixels) = reset_bind(args)?;
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
    merge_zygote_fields(&mut reply, session);
    // S3b: never a silent retry. The first host-transport failure and the path
    // to its preserved evidence travel with the successful reset reply.
    if let Some(retry) = session.relaunch_retry() {
        reply["relaunch_retry"] = retry.clone();
    }
    // FB2c item 3: the reset reply publishes the same first-frame evidence a
    // launch does. The reset bound the relaunched guest's first frame through
    // the launch's content wait and placeholder rule, so a relaunch that never
    // mounted is a `PLACEHOLDER_CONTENT` error (returned above), never a
    // `stage_reached: first_frame` pass.
    if let Some(report) = session.first_frame_report() {
        reply["first_frame"] = report.to_json();
        if report.placeholder || report.suspect {
            if let Some(frame) = reply.get_mut("frame").and_then(Value::as_object_mut) {
                frame.insert("meaningful".into(), json!(!report.placeholder));
                frame.insert("placeholder".into(), json!(report.placeholder));
            }
        }
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
    if (settle == "fast" || settle == "content") && pixels {
        return Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: format!(
                "settle:{settle} is a tree-only mode; its pixels may still be animating and \
                 cannot back a PNG capture (pass pixels:false, or settle:strict)"
            ),
        });
    }
    Ok((settle, pixels))
}

/// SE2b: `acos_reset` binds the relaunched guest's first frame with a
/// strict/fast traversal only. `settle:content` is refused as a structured
/// `INVALID_ARGUMENT` instead of being silently mapped to strict (it would
/// otherwise claim a rule the reset never applied).
fn reset_bind(args: &Value) -> Result<(&'static str, bool), BackendError> {
    let (settle, pixels) = launch_bind(args)?;
    if settle == "content" {
        return Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: "acos_reset does not bind a content-stable frame: the reset's first frame \
                     is strict (or fast navigation). Use settle:\"content\" on acos_launch, \
                     acos_observe/acos_observe_tree or an input verb instead"
                .to_string(),
        });
    }
    Ok((settle, pixels))
}

fn requested_settle(args: &Value) -> Result<&'static str, BackendError> {
    match args.get("settle").and_then(Value::as_str) {
        None | Some("strict") => Ok("strict"),
        Some("fast") => Ok("fast"),
        // SE2: the content-stable rule for a screen that animates forever in
        // pixels (ADR-0026 decision 3). Backs UI/text observations only.
        Some("content") => Ok("content"),
        Some(other) => Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: format!(
                "settle must be \"strict\", \"fast\" or \"content\" (got {other:?})"
            ),
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

/// FB3: parse the per-action `diff` flag. Default **on**; a non-bool is an
/// argument error, never silently ignored.
fn diff_requested(args: &Value) -> Result<bool, BackendError> {
    match args.get("diff") {
        None => Ok(true),
        Some(Value::Bool(value)) => Ok(*value),
        Some(other) => Err(BackendError::Failed {
            code: ErrorCode::InvalidArgument,
            detail: format!("`diff` must be a boolean (got {other})"),
        }),
    }
}

fn input_result(session: &mut ArtSession, kind: &str, target: Value) -> Value {
    // REL3E: the resolved target and its hit-test are the action's own evidence,
    // so they travel with the delivery the verifier reads (a scenario step's JSON
    // carries the same block the MCP reply does).
    session.attach_input_target(target.clone());
    let delivery = session.last_input_delivery();
    let settle = session.last_input_settle().cloned();
    let (delivered, state_changed, acks) = match &delivery {
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
    let handled = delivery.as_ref().and_then(|delivery| delivery.handled);
    let mut result = input_reply(
        kind,
        delivered,
        state_changed,
        handled,
        handled_evidence(&delivery),
        acks,
        target,
        settle,
    );
    // REL3E: the reply must answer "did the app handle it", not just "did we
    // send it". The hint is written against the *same* target block, so the
    // agent reads one object: what was resolved, what the point hit, what the
    // framework did with it.
    if let Some(object) = result.as_object_mut() {
        let hint = handling_hint(object.get("target"), handled, state_changed);
        if let Some(hint) = hint {
            object.insert("handled_hint".into(), json!(hint));
        }
    }
    // FB3: what the action changed, from the two trees the session already
    // holds (the pre-action tree and the tree this action's settle bound). Only
    // present when asked for: `diff:false` is the pre-FB3 reply, byte for byte.
    if session.diff_requested() {
        result["changes"] = serde_json::to_value(session.tree_changes()).unwrap_or(Value::Null);
    }
    result
}

/// REL3E: the delivery evidence behind `handled`, never a boolean on its own.
fn handled_evidence(delivery: &Option<acos_verify::backend::InputDelivery>) -> Value {
    let Some(delivery) = delivery else {
        return json!({"source": "none", "published": 0, "acked": 0});
    };
    let unhandled: Vec<u64> = delivery
        .acks
        .iter()
        .filter(|ack| !ack.handled)
        .map(|ack| ack.seq)
        .collect();
    json!({
        "source": "finishInputEvent",
        "published": delivery.published,
        "acked": delivery.acks.len(),
        "unhandled": unhandled,
    })
}

/// The action reply every backend has always produced. REL3E adds `handled` and
/// `handled_evidence` after `state_changed`, and nothing else: `target` keeps
/// its position and `diff:false` keeps every pre-REL3E key.
fn input_reply(
    kind: &str,
    delivered: bool,
    state_changed: bool,
    handled: Option<bool>,
    handled_evidence: Value,
    acks: Value,
    target: Value,
    settle: Option<Value>,
) -> Value {
    json!({
        "action": kind,
        "accepted": delivered,
        "state_changed": state_changed,
        "handled": handled,
        "handled_evidence": handled_evidence,
        "acks": acks,
        "target": target,
        "settle": settle,
        "input_evidence": "`handled` is the app window's own finishInputEvent(handled) answer (guest-asserted, one ack per InputMessage); state_changed is the host's own pixel+tree comparison in strict mode; in fast mode it is the guest's tree-change verdict and a fast frame may be mid-animation",
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

/// SE1/SE2b: `expect_change: true` is a **pixel** claim. Neither a fast frame
/// (mid-animation, never verification evidence) nor a content-settled frame
/// (content stable, pixels may still be animating) can measure it, so both are
/// rejected before any input is injected, exactly like the scenario parser
/// rejects `expect_change` + `settle: content` as `INVALID_SCENARIO`.
fn reject_expect_change_without_pixel_evidence(args: &Value) -> Result<(), BackendError> {
    let Some(settle) = args.get("settle").and_then(Value::as_str) else {
        return Ok(());
    };
    if !expect_change_arg(args)? {
        return Ok(());
    }
    let why = match settle {
        "fast" => {
            "a fast frame may be mid-animation and is never verification evidence; use \
             `settle: strict` (the default)"
        }
        "content" => {
            "a content-stable frame proves the UI tree content, never the pixels; a pixel-change \
             claim needs `settle: strict` (the default)"
        }
        _ => return Ok(()),
    };
    Err(BackendError::Failed {
        code: ErrorCode::InvalidArgument,
        detail: format!("`expect_change: true` cannot be combined with `settle: {settle}`: {why}"),
    })
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

/// Resolve an **assertion** selector to exactly one visible node.
///
/// Assertions stay field-exact: `text` matches a node's own text (or hint), never
/// its content description, so nothing that is a failure today can become a pass
/// (RNTAP's accessible-name rule is an *input* rule). What is shared with input
/// is the refusal to pick silently: several user-visible matches are
/// `AMBIGUOUS_SELECTOR` with the candidate list, not the first one.
fn resolve_assert_node(
    tree: &UiTree,
    selector: &Selector,
    describe: &Value,
) -> Result<UiNode, BackendError> {
    let candidates = tree.find_all(selector);
    match candidates.len() {
        1 => return Ok(candidates[0].clone()),
        0 => {}
        _ => return Err(ambiguous_selector(describe, &candidates)),
    }
    if let Some(offscreen) = tree.find_invisible(selector) {
        return Err(BackendError::Failed {
            code: ErrorCode::ElementOffscreen,
            detail: format!(
                "{describe} matches only a node that is not visible to the user (text {:?}, bounds {}, visible_bounds {}); scroll it into view first",
                offscreen.text,
                offscreen.bounds,
                offscreen
                    .visible_bounds
                    .map_or_else(|| "<none>".to_string(), |b| b.to_string())
            ),
        });
    }
    Err(BackendError::Failed {
        code: if tree.truncated {
            ErrorCode::TreeTruncated
        } else {
            ErrorCode::ElementNotFound
        },
        detail: format!(
            "no node matching {describe} in the live ART tree{}",
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
        "password": node.password,
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
    use super::{keep_log_record, log_level_rank};
    use acos_protocol::LogLevel;

    /// FB6 (5): the default log view is `W` + the app's own package.
    #[test]
    fn default_log_view_drops_framework_info_but_keeps_app_lines_and_warnings() {
        let pkg = Some("dev.acos.fixtures.counter");
        // Framework `I` noise before the app ever spoke.
        assert!(!keep_log_record(
            log_level_rank(LogLevel::Info),
            "SoLoader",
            "libworklets.so found",
            5,
            None,
            pkg
        ));
        // The app's own `I` line names the package: kept in the default view.
        assert!(keep_log_record(
            log_level_rank(LogLevel::Info),
            "AcosCounter",
            "dev.acos.fixtures.counter: onCreate",
            5,
            None,
            pkg
        ));
        // A warning is agent-relevant even when it names nothing.
        assert!(keep_log_record(
            log_level_rank(LogLevel::Warn),
            "ViewRootImpl",
            "sendUserActionEvent() returned",
            5,
            None,
            pkg
        ));
        // An error is kept.
        assert!(keep_log_record(
            log_level_rank(LogLevel::Error),
            "AndroidRuntime",
            "FATAL EXCEPTION",
            5,
            None,
            pkg
        ));
        // `min_priority: "V"` (floor 2) or `all: true` keeps everything, package or not.
        assert!(keep_log_record(
            log_level_rank(LogLevel::Verbose),
            "SoLoader",
            "noise",
            2,
            None,
            pkg
        ));
        // An explicit tag filter is a hard AND filter: nothing else survives it.
        assert!(!keep_log_record(
            log_level_rank(LogLevel::Error),
            "AndroidRuntime",
            "FATAL EXCEPTION",
            5,
            Some("acoscounter"),
            pkg
        ));
        // An explicit `tag` narrows but does not lift the priority floor: an
        // `I`-level record of that tag needs `min_priority: "V"` (floor 2) —
        // which is also what `all: true` selects.
        assert!(!keep_log_record(
            log_level_rank(LogLevel::Info),
            "AcosCounter",
            "onCreate",
            5,
            Some("acoscounter"),
            pkg
        ));
        assert!(keep_log_record(
            log_level_rank(LogLevel::Info),
            "AcosCounter",
            "onCreate",
            2,
            Some("acoscounter"),
            pkg
        ));
    }

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

    /// FB1: the live channel's selector precedence, field by field.
    #[test]
    fn live_selectors_prefer_id_then_content_desc_then_text() {
        let (selector, describe) = args_selector(&json!({
            "id": "search", "content_desc": "Search Japanese dictionary", "text": "kayou"
        }))
        .expect("a selector");
        assert_eq!(selector.id.as_deref(), Some("search"));
        assert_eq!(describe, json!({"id": "search"}));

        let (selector, describe) =
            args_selector(&json!({"content_desc": "Search Japanese dictionary", "text": "x"}))
                .expect("a selector");
        assert!(selector.id.is_none() && selector.text.is_none());
        assert_eq!(
            selector.content_desc.and_then(|t| t.exact).as_deref(),
            Some("Search Japanese dictionary")
        );
        assert_eq!(describe, json!({"content_desc": "Search Japanese dictionary"}));

        let (selector, describe) = args_selector(&json!({"text": "kayou", "index": 1}))
            .expect("a selector");
        assert_eq!(selector.text.and_then(|t| t.exact).as_deref(), Some("kayou"));
        assert_eq!(selector.index, Some(1), "index is ANDed with the field");
        assert_eq!(describe, json!({"text": "kayou", "index": 1}));

        // A point-only tap is not a selector.
        assert!(args_selector(&json!({"x": 10, "y": 20})).is_none());
    }

    /// FB1: two visible nodes with the same label are refused with both
    /// candidates in `details`, for input and for assertions.
    #[test]
    fn several_live_matches_are_refused_with_their_candidates() {
        let a = node("continue-a", "Continue", vec![]);
        let b = node("continue-b", "Continue", vec![]);
        let both = tree(vec![a, b]);
        let selector = Selector::by_text("Continue");
        let describe = json!({"text": "Continue"});

        let error = resolve_assert_node(&both, &selector, &describe)
            .expect_err("two matches must not resolve silently");
        match error {
            BackendError::Protocol {
                code, details, hint, ..
            } => {
                assert_eq!(code, ErrorCode::AmbiguousSelector);
                let details = details.expect("structured candidates");
                assert_eq!(details["count"], json!(2));
                let candidates = details["candidates"].as_array().expect("candidates");
                assert_eq!(candidates.len(), 2);
                assert_eq!(candidates[0]["resource_id"], json!("continue-a"));
                assert_eq!(candidates[1]["resource_id"], json!("continue-b"));
                assert_eq!(candidates[0]["text"], json!("Continue"));
                assert!(candidates[0]["bounds"].as_str().unwrap().contains("]["));
                assert!(hint.unwrap_or_default().contains("index"));
            }
            other => panic!("expected a structured ambiguous-selector error, got {other:?}"),
        }

        // One match still resolves; `index` picks explicitly.
        let single = tree(vec![node("only", "Continue", vec![])]);
        assert!(resolve_assert_node(&single, &selector, &describe).is_ok());
    }

    /// FB1: the reply's target is the node that received the input, as the tree
    /// shows it; the echo is masked only for a focused password node, or when
    /// the target is unknown, or when the caller forces it.
    fn typing_node(id: &str, focused: bool, password: bool) -> UiNode {
        let mut node = UiNode::new(id, "android.widget.EditText", Bounds::new(0, 0, 10, 10))
            .with_password(password)
            .with_focused(focused);
        node.window = "main".to_string();
        node
    }

    #[test]
    fn focused_search_field_is_echoed_and_named_as_the_target() {
        // Gaten's case: the search field holds focus; an unfocused credential
        // field sits on the same screen. The reply names the search field and
        // echoes the text -- never the credential field.
        let search = typing_node("search", true, false);
        let token = typing_node("token", false, true);
        let screen = tree(vec![search, token]);
        let decision = echo_decision(&screen, false);
        assert!(decision.mask_reason.is_none(), "a focused non-password field echoes");
        let target = input_target("kayou", &decision);
        assert_eq!(target["text"], json!("kayou"));
        assert!(target.get("text_masked").is_none());
        assert_eq!(target["node"]["id"], json!("search"));
        assert_eq!(target["node"]["password"], json!(false));
    }

    #[test]
    fn focused_password_field_is_masked_and_its_text_withheld() {
        let secret = typing_node("pw", true, true);
        let screen = tree(vec![secret]);
        let decision = echo_decision(&screen, false);
        assert!(decision.mask_reason.is_some());
        let target = input_target("s3cret", &decision);
        assert_eq!(target["text"], json!("*"));
        assert_eq!(target["text_masked"], json!(true));
        assert_eq!(target["chars"], json!(6));
        assert_eq!(target["node"]["id"], json!("pw"));
        assert_eq!(target["node"]["text"], Value::Null, "password text is withheld");
        assert!(!target.to_string().contains("s3cret"));
    }

    #[test]
    fn unknown_target_is_masked_and_names_no_node() {
        // No focused node at all: even a single password field elsewhere is not
        // substituted as the target; the reply withholds the text with the
        // reason "target unknown" and claims no node.
        let lone_password = typing_node("pw", false, true);
        let tree_with_password = tree(vec![lone_password]);
        let decision = echo_decision(&tree_with_password, false);
        let target = input_target("kayou", &decision);
        assert_eq!(target["text_masked"], json!(true));
        assert!(target["reason"].as_str().unwrap().starts_with("target unknown"));
        assert_eq!(target["node"], Value::Null, "no node is claimed");
        assert!(!target.to_string().contains("kayou"));

        let plain_unfocused = typing_node("plain", false, false);
        let plain_tree = tree(vec![plain_unfocused]);
        let decision = echo_decision(&plain_tree, false);
        assert!(decision.mask_reason.is_some(), "unknown target is masked by default");
    }

    #[test]
    fn mask_true_forces_masking_on_a_focused_plain_field() {
        let search = typing_node("search", true, false);
        let screen = tree(vec![search]);
        let decision = echo_decision(&screen, true);
        let target = input_target("abc", &decision);
        assert_eq!(target["text_masked"], json!(true));
        assert!(target["reason"].as_str().unwrap().starts_with("requested"));
        assert_eq!(target["node"]["id"], json!("search"));
        assert!(mask_requested(&json!({"mask": true})).unwrap());
        assert!(!mask_requested(&json!({"text": "x"})).unwrap());
        assert!(mask_requested(&json!({"mask": "yes"})).is_err());
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

    /// FB3: `diff:false` must be the pre-FB3 reply, byte for byte; the diff is
    /// only ever an *added* key after the existing ones. REL3E adds `handled`
    /// and `handled_evidence`, and the byte-exact expectation carries them.
    #[test]
    fn fb3_diff_false_reply_is_byte_identical_and_changes_are_appended() {
        let base = input_reply(
            "tap",
            true,
            true,
            Some(true),
            handled_evidence(&None),
            json!([{"seq": 3, "handled": true, "latency_us": 1200}]),
            json!({"id": "inc"}),
            Some(json!({"mode": "strict", "settled": true})),
        );
        let expected = concat!(
            "{\"accepted\":true,\"acks\":[{\"handled\":true,\"latency_us\":1200,\"seq\":3}],",
            "\"action\":\"tap\",\"handled\":true,",
            "\"handled_evidence\":{\"acked\":0,\"published\":0,\"source\":\"none\"},",
            "\"input_evidence\":\"`handled` is the app window's own ",
            "finishInputEvent(handled) answer (guest-asserted, one ack per InputMessage); ",
            "state_changed is the host's own pixel+tree comparison in strict mode; in fast ",
            "mode it is the guest's tree-change verdict and a fast frame may be mid-animation\",",
            "\"settle\":{\"mode\":\"strict\",\"settled\":true},\"state_changed\":true,",
            "\"target\":{\"id\":\"inc\"}}"
        );
        assert_eq!(serde_json::to_string(&base).unwrap(), expected);
        // The diff path only adds one key: strip it and the reply is the same
        // object, byte for byte.
        let mut with_changes = base.clone();
        with_changes["changes"] = json!({"baseline": "none"});
        let serialized = serde_json::to_string(&with_changes).unwrap();
        assert!(
            serialized.contains("\"changes\":{\"baseline\":\"none\"}"),
            "the changes block must be present: {serialized}"
        );
        let mut round_tripped: Value = serde_json::from_str(&serialized).unwrap();
        round_tripped
            .as_object_mut()
            .unwrap()
            .remove("changes")
            .expect("the diff is the only added key");
        assert_eq!(
            serde_json::to_string(&round_tripped).unwrap(),
            expected,
            "removing `changes` must give back the pre-FB3 reply exactly"
        );
    }

    /// REL3E: a tap reply answers "did the app handle it", and the target block
    /// names what the point hit plus the clickable node to tap instead.
    #[test]
    fn rel3e_tap_target_names_the_hit_and_the_clickable_sibling() {
        // The Gaten settings row: a non-clickable label whose clickable switch
        // sits beside it, inside a row that is not clickable either.
        let mut label = UiNode::new("n1", "android.widget.TextView", Bounds::new(40, 300, 460, 360))
            .with_resource_id("title")
            .with_text("Floating button");
        label.window = "main".into();
        let mut switch = UiNode::new("n2", "android.widget.Switch", Bounds::new(900, 300, 1040, 360))
            .with_resource_id("toggle")
            .with_text("Floating button");
        switch.clickable = true;
        switch.checked = false;
        switch.window = "main".into();
        let mut row = UiNode::new("n0", "android.widget.LinearLayout", Bounds::new(0, 296, 1080, 364));
        row.window = "main".into();
        row.children = vec![label, switch];
        let screen = tree(vec![row]);

        let target = tap_target(
            Some(&json!({"text": "Floating button"})),
            None,
            Some(&screen),
            250,
            330,
        );
        assert_eq!(target["selector"], json!({"text": "Floating button"}));
        assert_eq!(target["point"], json!({"x": 250, "y": 330}));
        assert_eq!(target["hit"]["method"], json!("tree-geometry"));
        assert_eq!(target["hit"]["node"]["resource_id"], json!("title"));
        assert_eq!(target["hit"]["node"]["class"], json!("android.widget.TextView"));
        assert_eq!(target["hit"]["node"]["bounds"], json!("[40,300][460,360]"));
        assert_eq!(target["hit"]["clickable"], json!(false));
        assert_eq!(target["hit"]["consumer"], Value::Null, "no ancestor is clickable");
        assert_eq!(target["hit"]["suggest"]["resource_id"], json!("toggle"));
        assert_eq!(target["hit"]["suggest"]["clickable"], json!(true));

        let hint = handling_hint(Some(&target), Some(false), false).expect("a hint");
        assert!(hint.contains("nothing tappable there"), "{hint}");
        assert!(hint.contains("Floating button"), "{hint}");
        assert!(hint.contains("android.widget.Switch"), "{hint}");
        assert!(hint.contains("nearest clickable"), "{hint}");
        assert!(hint.contains("finishInputEvent(handled) = false"), "{hint}");
    }

    /// REL3E: a hit on a clickable control needs no suggestion, and a handled
    /// input with no visible change explains itself.
    #[test]
    fn rel3e_handled_true_with_or_without_a_change() {
        let mut button = UiNode::new("n1", "android.widget.Button", Bounds::new(0, 0, 100, 60))
            .with_resource_id("inc")
            .with_text("Increment");
        button.clickable = true;
        let screen = tree(vec![button]);
        let target = tap_target(None, None, Some(&screen), 50, 30);
        assert_eq!(target["hit"]["node"]["resource_id"], json!("inc"));
        assert_eq!(target["hit"]["clickable"], json!(true));
        assert_eq!(target["hit"]["consumer"]["resource_id"], json!("inc"));
        assert_eq!(target["hit"]["suggest"], Value::Null);
        assert_eq!(handling_hint(Some(&target), Some(true), true), None);
        let hint = handling_hint(Some(&target), Some(true), false).expect("a hint");
        assert!(hint.contains("consumed the input"), "{hint}");
        assert!(hint.contains("no change"), "{hint}");
    }

    /// REL3E: a missing ack is "unknown", never "not handled"; a point with no
    /// node and no tree is described as such.
    #[test]
    fn rel3e_unknown_handling_is_reported_as_unknown() {
        let hint = handling_hint(None, None, false).expect("a hint");
        assert!(hint.contains("unknown"), "{hint}");
        assert!(!hint.contains("did NOT consume"), "{hint}");

        let empty = tree(vec![UiNode::new("n0", "android.view.View", Bounds::new(0, 0, 10, 10))]);
        let target = tap_target(None, None, Some(&empty), 5, 5);
        assert_eq!(target["hit"]["node"]["class"], json!("android.view.View"));
        let hint = handling_hint(Some(&target), Some(false), false).expect("a hint");
        assert!(hint.contains("nothing tappable there"), "{hint}");
        assert!(hint.contains("no clickable node"), "{hint}");

        let no_tree = tap_target(None, None, None, 5, 5);
        assert_eq!(no_tree["hit"]["node"], Value::Null);
        assert_eq!(no_tree["hit"]["clickable"], Value::Null);
        assert!(no_tree["hit"]["note"].as_str().unwrap().contains("no bound tree"));
        let hint = handling_hint(Some(&no_tree), Some(false), false).expect("a hint");
        assert!(hint.contains("no node there"), "{hint}");
    }

    /// REL3E: `target` for a resolved tap carries the node's input fields, and
    /// the delivery evidence counts published vs acked events.
    #[test]
    fn rel3e_reply_carries_handled_and_evidence() {
        let mut button = UiNode::new("n1", "android.widget.Button", Bounds::new(0, 0, 100, 60))
            .with_resource_id("inc")
            .with_text("Increment");
        button.clickable = true;
        button.focused = true;
        button.enabled = false;
        let target = tap_target(Some(&json!({"id": "inc"})), Some(&button), None, 50, 30);
        let node = &target["node"];
        assert_eq!(node["id"], json!("n1"));
        assert_eq!(node["class"], json!("android.widget.Button"));
        assert_eq!(node["text"], json!("Increment"));
        assert_eq!(node["content_desc"], json!(""));
        assert_eq!(node["bounds"], json!("[0,0][100,60]"));
        assert_eq!(node["clickable"], json!(true));
        assert_eq!(node["enabled"], json!(false));
        assert_eq!(node["focused"], json!(true));

        let evidence = handled_evidence(&None);
        assert_eq!(evidence["source"], json!("none"));
        assert_eq!(evidence["published"], json!(0));
        assert_eq!(evidence["acked"], json!(0));
    }

    /// FB3: `diff` defaults on and a non-boolean is a typed argument error.
    #[test]
    fn fb3_diff_defaults_on_and_rejects_non_booleans() {
        assert!(diff_requested(&json!({})).unwrap());
        assert!(diff_requested(&json!({"diff": true})).unwrap());
        assert!(!diff_requested(&json!({"diff": false})).unwrap());
        let err = diff_requested(&json!({"diff": "no"})).unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidArgument);
        assert!(
            err.to_string().contains("`diff` must be a boolean"),
            "{err}"
        );
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
    fn fb2_scratch(name: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/fb2-tests");
        let dir = root.join(format!(
            "{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // `CARGO_MANIFEST_DIR/../..` keeps `..` components, which compare
        // component-wise against a canonicalized reply path; resolve it once.
        std::fs::canonicalize(&dir).unwrap_or(dir)
    }

    /// FB2: `screenshot: true` writes the frame's (already compressed) PNG under
    /// the session's artifacts and reports an absolute path, the pixel geometry
    /// and `display_origin`.
    #[test]
    fn a_screenshot_request_writes_a_png_and_reports_its_geometry() {
        let dir = fb2_scratch("shotz");
        let source = dir.join("frame.png");
        std::fs::write(&source, crate::engine::fake::TINY_PNG).unwrap();
        let artifacts = dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let mut result = json!({"node_count": 9});
        write_screenshot(
            &artifacts,
            None,
            "acos_observe_tree",
            (&source, 7, "m7", crate::engine::fake::TINY_PNG.len() as u64),
            (1080, 2400),
            (0, 128),
            &mut result,
        )
        .expect("the screenshot is written");
        let shot = &result["screenshot"];
        let path = std::path::Path::new(shot["path"].as_str().unwrap());
        assert!(path.is_absolute(), "the reply names an absolute path: {shot}");
        assert_eq!(
            path.file_name().unwrap(),
            "observe_tree-7.png",
            "the default name carries the verb and the frame sequence: {shot}"
        );
        assert!(
            path.starts_with(&artifacts),
            "the default destination is the session's own artifacts dir: {shot}"
        );
        assert_eq!(
            std::fs::read(path).unwrap(),
            crate::engine::fake::TINY_PNG,
            "the PNG is copied byte for byte (never re-encoded)"
        );
        assert_eq!(shot["format"], json!("png"));
        assert_eq!(shot["frame_seq"], json!(7));
        assert_eq!(shot["md5"], json!("m7"));
        assert_eq!(shot["bytes"], json!(crate::engine::fake::TINY_PNG.len()));
        assert_eq!(shot["display_size"], json!([1080, 2400]));
        assert_eq!(
            shot["display_origin"],
            json!([0, 128]),
            "the reply carries the offset between PNG pixels and tree bounds"
        );
        // The pixel size comes from the PNG's own IHDR, not from the display.
        let (w, h) = png_pixel_size(path).expect("a real PNG header");
        assert_eq!(shot["width"], json!(w));
        assert_eq!(shot["height"], json!(h));
        assert!(
            shot["note"].as_str().unwrap().contains("display_origin"),
            "{shot}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FB2/FB2c: an explicit path is honoured (the CLI's `--screenshot <path>`
    /// form) and made absolute; a **relative** path resolves against the
    /// session's artifacts directory, never the daemon's cwd.
    #[test]
    fn a_screenshot_path_argument_is_honoured_and_made_absolute() {
        let dir = fb2_scratch("path");
        let source = dir.join("frame.png");
        std::fs::write(&source, crate::engine::fake::TINY_PNG).unwrap();
        let artifacts = dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let dest = artifacts.join("out/agent.png");
        let mut result = json!({});
        write_screenshot(
            &artifacts,
            Some(PathBuf::from("out/agent.png")),
            "acos_observe",
            (&source, 3, "m3", crate::engine::fake::TINY_PNG.len() as u64),
            (1080, 2400),
            (0, 0),
            &mut result,
        )
        .unwrap();
        assert_eq!(
            result["screenshot"]["path"],
            json!(std::fs::canonicalize(&dest).unwrap().display().to_string())
        );
        assert!(dest.is_file(), "a relative path lands under the artifacts dir");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FB2c item 5: the screenshot destination is caller input, so it is confined
    /// to the session's artifacts directory: `..`, an absolute path outside it, a
    /// symlink that leaves it and the frame artifact itself are all refused, and a
    /// refusal writes nothing.
    #[test]
    fn a_screenshot_destination_cannot_leave_the_session_artifacts() {
        let dir = fb2_scratch("confine");
        let source = dir.join("frame.png");
        std::fs::write(&source, crate::engine::fake::TINY_PNG).unwrap();
        let artifacts = dir.join("artifacts");
        std::fs::create_dir_all(artifacts.join("screenshots")).unwrap();
        let attempt = |dest: Option<PathBuf>| {
            let mut result = json!({});
            write_screenshot(
                &artifacts,
                dest,
                "acos_observe",
                (&source, 3, "m3", crate::engine::fake::TINY_PNG.len() as u64),
                (1080, 2400),
                (0, 0),
                &mut result,
            )
        };
        for (why, dest) in [
            ("a `..` component", PathBuf::from("../../../escape.png")),
            ("an absolute path outside", dir.join("outside.png")),
            ("a `..` even when it lands inside", PathBuf::from("screenshots/../x.png").join("..").join("y.png")),
        ] {
            let error = attempt(Some(dest)).expect_err(why);
            match error {
                BackendError::Error { code, detail } => {
                    assert_eq!(code, ErrorCode::InvalidArgument, "{why}: {detail}");
                    assert!(detail.contains("artifacts directory"), "{why}: {detail}");
                }
                other => panic!("{why}: expected an argument error, got {other:?}"),
            }
        }
        // A symlink inside the artifacts dir that points outside it.
        let link = artifacts.join("screenshots/escape.png");
        std::os::unix::fs::symlink(dir.join("target-dir"), &link).unwrap();
        std::fs::create_dir_all(dir.join("target-dir")).unwrap();
        let error = attempt(Some(PathBuf::from("screenshots/escape.png/x.png")))
            .expect_err("a symlink out of the directory is refused");
        assert!(matches!(error, BackendError::Error { .. }), "{error:?}");
        assert!(
            !dir.join("target-dir/x.png").exists(),
            "nothing may be created through the symlink"
        );
        // The frame artifact itself: copying it onto itself truncates it to 0.
        let error = attempt(Some(source.clone())).expect_err("the source is refused");
        assert!(matches!(error, BackendError::Error { .. }), "{error:?}");
        assert_eq!(
            std::fs::read(&source).unwrap(),
            crate::engine::fake::TINY_PNG,
            "the frame artifact is untouched"
        );
        // Nothing was created outside the artifacts dir by any refusal.
        assert!(!dir.join("escape.png").exists());
        assert!(!dir.join("outside.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_screenshot_argument_is_true_or_a_path_and_nothing_else() {
        assert_eq!(
            screenshot_request(&json!({})).unwrap(),
            ScreenshotRequest::None
        );
        assert_eq!(
            screenshot_request(&json!({"screenshot": false})).unwrap(),
            ScreenshotRequest::None
        );
        assert_eq!(
            screenshot_request(&json!({"screenshot": true})).unwrap(),
            ScreenshotRequest::Default
        );
        assert_eq!(
            screenshot_request(&json!({"screenshot": "out/x.png"})).unwrap(),
            ScreenshotRequest::Path(PathBuf::from("out/x.png"))
        );
        // A value the verb cannot honour is an argument error, never a silent
        // no-op (the reported bug: no file, no error, no screenshot field).
        let err = screenshot_request(&json!({"screenshot": 7})).unwrap_err();
        match err {
            BackendError::Error { code, detail } => {
                assert_eq!(code, ErrorCode::InvalidArgument);
                assert!(detail.contains("`screenshot` must be"), "{detail}");
            }
            other => panic!("expected an argument error, got {other:?}"),
        }
        let err = screenshot_request(&json!({"screenshot": ""})).unwrap_err();
        assert!(matches!(err, BackendError::Error { .. }));
    }

    /// FB2: a placeholder/no-pixel frame is a structured `NO_FRESH_FRAME`, not an
    /// empty success.
    #[test]
    fn a_placeholder_frame_has_no_screenshot_to_give() {
        let err = placeholder_screenshot_error("acos_observe_tree");
        match err {
            BackendError::Error { code, detail } => {
                assert_eq!(code, ErrorCode::NoFreshFrame);
                assert_eq!(code.as_str(), "NO_FRESH_FRAME");
                assert!(detail.contains("placeholder"), "{detail}");
                assert!(detail.contains("acos_observe_tree"), "{detail}");
            }
            other => panic!("expected NO_FRESH_FRAME, got {other:?}"),
        }
    }

    /// FB2: a launch that reached only the window skeleton is `blocked` with
    /// reason `placeholder-content` — never `pass` / `first_frame`.
    fn placeholder_report(nodes: usize, placeholder: bool) -> crate::engine::art::FirstFrameReport {
        crate::engine::art::FirstFrameReport {
            traversals: 2,
            waited_ms: 5003,
            event_waits: 1,
            settled: true,
            bound: Some("strict-event".to_string()),
            nodes,
            placeholder,
            suspect: false,
            frame_seq: Some(7),
            frame_number: Some(1),
            frames: 2,
            frames_advanced: false,
            pixels_compared: true,
            pixels_changed: false,
            budget_ms: 5000,
        }
    }

    #[test]
    fn a_placeholder_launch_is_blocked_not_a_pass() {
        let error = placeholder_launch_error(
            "s_gaten",
            &placeholder_report(9, true),
            Some(1),
            Some("E SoLoader: couldn't find libworklets.so".to_string()),
        );
        let envelope = Envelope::failure(
            "req_fb2",
            ProtocolBackend::AcosNative,
            Fidelity::ArtReference,
            error.clone(),
            Metrics::wall_ms(0),
        );
        assert!(!envelope.status().is_pass(), "a placeholder is never a pass");
        assert_eq!(envelope.status(), acos_protocol::Status::Blocked);
        assert_eq!(error.code, ErrorCode::PlaceholderContent);
        assert_eq!(error.code.as_str(), "PLACEHOLDER_CONTENT");
        assert_eq!(error.details.as_ref().unwrap()["reason"], json!("placeholder-content"));
        assert_eq!(error.details.as_ref().unwrap()["nodes"], json!(9));
        assert_eq!(error.details.as_ref().unwrap()["frame_number"], json!(1));
        assert_eq!(error.details.as_ref().unwrap()["session_id"], json!("s_gaten"));
        // The hint carries the log line that names the cause, so an agent does
        // not have to guess why the content never mounted.
        let hint = error.hint.clone();
        assert!(
            hint.contains("libworklets.so"),
            "the hint quotes the explaining log line: {hint}"
        );
        assert!(hint.contains("acos_inspect"), "{hint}");
        assert!(
            error.message.contains("placeholder-content"),
            "the message names the reason: {}",
            error.message
        );
        // FB2c: the budget and its env knob are named, and the frame number is
        // quoted as a *measured* fact, never asserted as "did not advance" when
        // nothing measured it.
        assert!(
            error.message.contains("ACOS_ART_FIRST_FRAME_MS=5000"),
            "the message names the budget it spent: {}",
            error.message
        );
        assert!(
            error.message.contains("frame_number stayed 1"),
            "a measured frame number is quoted: {}",
            error.message
        );
        assert!(
            error.hint.contains("ACOS_ART_FIRST_FRAME_MS"),
            "the hint says a longer timeout cannot help: {}",
            error.hint
        );

        // The same facts with no explaining log line still name the reason and
        // point at the log, never at a guessed cause.
        let bare = placeholder_launch_error("s_x", &placeholder_report(9, true), None, None);
        assert_eq!(bare.code, ErrorCode::PlaceholderContent);
        assert!(bare.hint.contains("acos_logs"), "{}", bare.hint);
        assert_eq!(
            bare.details.as_ref().unwrap().get("log_hint"),
            None,
            "no guessed cause"
        );
        // A report that never measured a frame number says so instead of
        // claiming it did not advance.
        let mut unmeasured = placeholder_report(9, true);
        unmeasured.frame_number = None;
        let unmeasured = placeholder_launch_error("s_y", &unmeasured, None, None);
        assert!(
            !unmeasured.message.contains("did not advance"),
            "an unmeasured frame number is never asserted: {}",
            unmeasured.message
        );
    }

    #[test]
    fn the_placeholder_hint_line_prefers_the_missing_native_library() {
        let lines = vec![
            "I SoLoader: libappmodules.so not found on /data/app/lib/x86_64".to_string(),
            "E SoLoader: couldn't find libworklets.so".to_string(),
            r#"E SoLoader: java.lang.UnsatisfiedLinkError: dlopen failed: library "libworklets.so" not found"#
                .to_string(),
            "E unknown: FATAL EXCEPTION: mqt_native_modules".to_string(),
        ];
        assert_eq!(
            placeholder_hint_line(&lines).as_deref(),
            Some(
                r#"E SoLoader: java.lang.UnsatisfiedLinkError: dlopen failed: library "libworklets.so" not found"#
            )
        );
        // A SoLoader "couldn't find" line is next, then a fatal runtime line,
        // then any error-level line, then nothing.
        let without_symbol = [lines[1].clone(), lines[3].clone()];
        assert_eq!(
            placeholder_hint_line(&without_symbol).as_deref(),
            Some("E SoLoader: couldn't find libworklets.so")
        );
        assert_eq!(
            placeholder_hint_line(&lines[3..]).as_deref(),
            Some("E unknown: FATAL EXCEPTION: mqt_native_modules")
        );
        assert_eq!(
            placeholder_hint_line(&["W Choreographer: skipped 32 frames".to_string()]).as_deref(),
            None
        );
        assert_eq!(placeholder_hint_line(&[]), None);
    }

    /// The normal path is untouched: a content report is not turned into a
    /// failure by any of the placeholder machinery.
    #[test]
    fn a_content_launch_stays_the_normal_pass_path() {
        let report = placeholder_report(243, false);
        let envelope = Envelope::pass(
            "req_fb2_ok",
            ProtocolBackend::AcosNative,
            Fidelity::ArtReference,
            Metrics::wall_ms(0),
        );
        assert!(envelope.status().is_pass());
        assert!(
            !report.placeholder,
            "content must never be labelled a placeholder"
        );
    }

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
        // SE2: content settle is tree-content evidence only; a PNG claim from it
        // is refused exactly like fast.
        assert_eq!(
            launch_bind(&json!({"settle": "content"})).unwrap(),
            ("content", false)
        );
        let err = launch_bind(&json!({"settle": "content", "pixels": true})).unwrap_err();
        assert!(
            err.to_string().contains("tree-only mode"),
            "content+pixels must be refused, got {err}"
        );
    }

    /// SE2b finding 2: a content-mode `acos_assert` must take a **new** content
    /// settle, never the cached bound tree. Before the fix the assert served the
    /// sticky cache, so it could pass on a screen that had moved since the
    /// content step.
    #[test]
    fn a_content_assert_reproves_the_settle_instead_of_reading_the_cache() {
        let content = json!({"mode": "content", "settled": true, "frame_seq": 5});
        assert!(
            assert_needs_fresh_content_settle(Some(&content)),
            "a content-bound assert must re-settle"
        );
        // An *unsettled* content record is still a content session (the rule is
        // the one in force) - re-settling is what says whether it holds now.
        let unsettled = json!({"mode": "content", "settled": false});
        assert!(assert_needs_fresh_content_settle(Some(&unsettled)));
        // Strict/fast sessions keep their single traversal.
        assert!(!assert_needs_fresh_content_settle(Some(
            &json!({"mode": "strict", "settled": true})
        )));
        assert!(!assert_needs_fresh_content_settle(Some(
            &json!({"mode": "fast", "settled": true})
        )));
        assert!(!assert_needs_fresh_content_settle(None));
    }

    /// SE2b finding 6: `acos_reset{settle:"content"}` must be a structured
    /// INVALID_ARGUMENT, never a silent mapping to strict.
    #[test]
    fn reset_refuses_a_content_settle_instead_of_silently_binding_strict() {
        let err = reset_bind(&json!({"settle": "content"})).expect_err("content reset is refused");
        assert_eq!(err.code(), ErrorCode::InvalidArgument);
        assert!(
            err.to_string().contains("does not bind a content-stable frame"),
            "the refusal must say why: {err}"
        );
        // The accepted shapes are unchanged.
        assert_eq!(reset_bind(&json!({})).unwrap(), ("strict", false));
        assert_eq!(reset_bind(&json!({"settle": "strict"})).unwrap(), ("strict", false));
        assert_eq!(reset_bind(&json!({"settle": "fast"})).unwrap(), ("fast", false));
    }

    /// SE2b finding 7: `expect_change` is a pixel claim, so BOTH unmeasured
    /// settle rules are refused up front (before any input is injected) - the
    /// content combination was previously refused only by accident, at the
    /// pixel-change check after the tap.
    #[test]
    fn expect_change_is_refused_with_every_unmeasured_settle() {
        let err = reject_expect_change_without_pixel_evidence(
            &json!({"settle": "content", "expect_change": true}),
        )
        .expect_err("content + expect_change is a pixel claim with no pixel evidence");
        assert_eq!(err.code(), ErrorCode::InvalidArgument);
        assert!(err.to_string().contains("settle: content"), "{err}");
        let err = reject_expect_change_without_pixel_evidence(
            &json!({"settle": "fast", "expect_change": true}),
        )
        .expect_err("fast + expect_change is refused as before");
        assert_eq!(err.code(), ErrorCode::InvalidArgument);
        // Strict (the default) still measures the pixels, and a content step
        // that does not claim a pixel change is fine.
        assert!(reject_expect_change_without_pixel_evidence(
            &json!({"settle": "strict", "expect_change": true})
        )
        .is_ok());
        assert!(reject_expect_change_without_pixel_evidence(
            &json!({"settle": "content"})
        )
        .is_ok());
        assert!(reject_expect_change_without_pixel_evidence(&json!({"expect_change": true})).is_ok());
    }
}
