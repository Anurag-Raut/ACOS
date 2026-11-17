//! `network` — real internet for the session (ADR-0030), as a service module.
//!
//! The transport is `acos-netd` (design B2): one relay netns and one `pasta`
//! per host, one netns + veth per session, `dnsproxyd` served by the launcher,
//! and `AF_INET`/`AF_INET6` in the guest's seccomp program only while this
//! module is attached. This module is the **switch and the control surface**:
//!
//! * `off` (the default: not attached) is the honest no-network answer the
//!   framework already gives — a sandbox with no route, `LinkProperties` with no
//!   address, no validated `NetworkCapabilities`, a resolver with nothing to ask.
//! * `real` is the relay arm: the guest runs inside the session netns, its
//!   `getaddrinfo` reaches `dnsproxyd`, and an `HttpURLConnection` fetch gets the
//!   real page. `ConnectivityManager` then describes the interface the namespace
//!   *actually* has (`crate::network` is the gate `crate::connectivity` reads).
//!
//! There is **no `stub` or `scripted` profile**, and that is a decision, not a
//! gap: a scripted network would be a fake interface (`gettaddrinfo` answering
//! from a table), which is exactly the "never a fake success" rule ADR-0029
//! item 3 forbids. Determinism comes from the real arm's own controls instead —
//! `offline`/`online` drop and restore this session's traffic on its own veth,
//! `delay` shapes it (`ms`, `jitter_ms`, `loss_pct`), and `hostport` is the
//! explicit host-loopback opt-in. Those are the `acos_net` actions, and they are
//! this module's [`ControlOp`]s: **one implementation**, two spellings
//! (`acos_net {action}` and `acos_service_ctl {module: "network", op: <action>}`).
//!
//! # Attach is at launch, and that is not a shortcut
//!
//! A network namespace is joined by a *process* at exec. A guest that is already
//! running cannot be moved into the session netns, its seccomp program is
//! installed at exec, and `pasta`'s veth is attached to the guest's netns by pid.
//! So `attach: at-launch` is the honest mode: `acos_services {op: "attach"}`
//! answers `SERVICE_MODULE_REJECTED` (ADR-0029's rule for an `at-launch` module),
//! never a half-attached network. The *controls* are hot, which is what an agent
//! needs mid-run.
//!
//! # Budget
//!
//! Declared `ram_mib: 2`, `startup_ms: 100`. Measured (NET4, `results/handoff/
//! NET4/REPORT.md` §1): the relay itself is ~26 MiB **once per host** (one
//! `pasta` for every session) and ~0 MiB per session; the per-session attach
//! (netns + veth + rules) is 25–65 ms, and NET4 left the arm at **+130 ms** over
//! module-off session boot (0.28 s vs 0.15 s at N=1). NET5's budget is the
//! target, measured in `results/handoff/NET5/TIMING.md`; the report says
//! `basis: "measured"` only where a number was measured.

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::host::ServiceHost;
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};
use crate::netd::client::Client;
use crate::netd::net_ctl::NetAction;
use crate::netd::proto::{ErrorCode as NetCode, ProtoError};

/// The module id (`acos_launch {"services": [{"id": "network"}]}`).
pub const MODULE_ID: &str = "network";

/// The launcher publishes the session id in this variable (NET4's one rule:
/// the relay session is `acos-<id>`). The module reads it at attach so a control
/// op addresses **this** session's network, never "whatever the relay has".
pub const SESSION_ID_ENV: &str = "ACOS_ART_SESSION_ID";

/// The `ServiceManager` name this module owns (and `core` answers while it is
/// off): `ConnectivityManager`'s own service.
const CONNECTIVITY_NAME: &str = "connectivity";
/// Its AIDL descriptor (`android.net.IConnectivityManager`).
const CONNECTIVITY_DESCRIPTOR: &str = "android.net.IConnectivityManager";

/// `ModuleConfig` key that names the session when a caller (a test, the Rust VM)
/// has no launcher environment to read.
pub const CONFIG_SESSION: &str = "session";

/// The control ops: exactly the `acos_net` actions.
///
/// `acos_net {action: "offline"}` and
/// `acos_service_ctl {module: "network", op: "offline"}` reach the same
/// [`Module::control`] arm; `crates/acos-mcp/tests/net_ctl_parity.rs` pins that
/// the two cannot drift.
const CONTROL_OPS: [ControlOp; 5] = [
    ControlOp {
        name: "status",
        args: "none — the relay state and this session's own row (`networked`, `offline`, \
               `delay_ms`, `loss_pct`, `host_ports`)",
    },
    ControlOp {
        name: "offline",
        args: "none — drop this session's traffic (its own veth only)",
    },
    ControlOp {
        name: "online",
        args: "none — restore it",
    },
    ControlOp {
        name: "delay",
        args: "{ms, jitter_ms, loss_pct} — shape this session's veth; `0/0/0` clears it",
    },
    ControlOp {
        name: "hostport",
        args: "{port, on?} — the explicit host-loopback opt-in (Metro's `localhost`)",
    },
];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "real internet through the acos-netd relay (one pasta per host, one netns+veth \
                  per session, dnsproxyd in the guest); hot online/offline/latency/loss controls and \
                  the explicit host-port opt-in; no `stub`/`scripted` profile (a fake network would \
                  be a fake success)",
        // The module owns the *answer* `ConnectivityManager` gives: `real` is a
        // validated internet network with the interface the session netns really
        // has, and while the module is off `core` answers the honest offline
        // device ACOS has always presented (ADR-0029's `replaces`: `core` serves
        // the name only while the module is off, and detach puts core's
        // registration back). The transport itself is host-side (the relay), so
        // there is no second name to claim: `netd`/`dnsresolver` are not services
        // this build has, and inventing them would be the fake this framework
        // forbids.
        services: &[ServiceProvide {
            name: CONNECTIVITY_NAME,
            descriptor: CONNECTIVITY_DESCRIPTOR,
        }],
        replaces: &["connectivity"],
        process_initializers: &[],
        features: &["android.hardware.wifi"],
        providers: &[],
        broadcasts: &[],
        windows: &[],
        system_apps: &[],
        requires: &[],
        requires_host_tools: &["pasta", "acos-netd"],
        // `stub`/`scripted` are deliberately absent (see the module docs).
        profiles: &[Profile::Real],
        budget: Budget {
            ram_mib: 2,
            startup_ms: 100,
        },
        // A running guest cannot be moved into a netns: see the module docs.
        attach: AttachMode::AtLaunch,
        controls: &CONTROL_OPS,
        security: SecurityClass::HostTool,
    };
    &MANIFEST
}

/// The `network` module.
pub struct NetworkModule {
    profile: Profile,
    /// `acos-<session id>`: the relay session every control addresses.
    relay_session: Option<String>,
    /// The `IConnectivityManager` state this module's registration serves.
    ///
    /// The module replaces `core`'s registration (the manifest's `replaces`), so
    /// the requests/callbacks an app makes after the attach are this module's. A
    /// module attached at launch swaps it before the app can call, which is why
    /// `at-launch` is the honest attach mode here.
    connectivity: Option<std::sync::Arc<std::sync::Mutex<crate::connectivity::ConnectivityState>>>,
}

impl NetworkModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            relay_session: None,
            connectivity: None,
        }
    }

    /// The manifest (registry and tests).
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }

    /// The relay session this module drives (`acos-<id>`), if attached.
    #[must_use]
    pub fn relay_session(&self) -> Option<&str> {
        self.relay_session.as_deref()
    }

    fn session(&self) -> Result<&str> {
        self.relay_session.as_deref().ok_or(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: "this module has no session id, so it cannot address a relay session; a \
                     `real` profile needs one (the launcher publishes ACOS_ART_SESSION_ID, or pass \
                     `config: {\"session\": \"<id>\"}`)"
                .to_string(),
        })
    }
}

impl Default for NetworkModule {
    fn default() -> Self {
        Self::new()
    }
}

/// Map the relay's protocol error onto the service layer's structured errors.
///
/// The codes matter to the agent: a relay that is down is
/// [`codes::NETWORK_UNAVAILABLE`] (fail closed — "this session has no network"),
/// never a silent offline, and a malformed control is
/// [`codes::SERVICE_MODULE_REJECTED`].
fn relay_error(error: ProtoError, session: Option<&str>) -> ServiceError {
    let name = session.unwrap_or("<unattached>");
    match error.code {
        NetCode::NetworkUnavailable => ServiceError::NetworkUnavailable {
            detail: format!(
                "{}; start it with `acos-netd up` (relay session `{name}`)",
                error.message
            ),
        },
        NetCode::NoSuchSession => ServiceError::NetworkNoSuchSession {
            detail: format!("{} (relay session `{name}`)", error.message),
        },
        NetCode::Invalid => ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: error.message,
        },
        _ => ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("{} ({:?}, relay session `{name}`)", error.message, error.code),
        },
    }
}

/// Run one `acos_net` action against this session's relay row.
///
/// This is the **only** implementation of the verb: [`Module::control`] calls it
/// for `acos_service_ctl`, and `acos-cli`'s `acos_net` arm routes to
/// `Module::control`, so the two spellings cannot diverge.
fn run_action(action: &NetAction, relay_session: &str) -> Result<Value> {
    let mut c = Client::connect().map_err(|e| match e.code {
        NetCode::NetworkUnavailable | NetCode::SupervisorGone => ServiceError::NetworkUnavailable {
            detail: format!(
                "the network relay supervisor is not running ({}); start it with `acos-netd up` \
                 (relay session `{relay_session}`)",
                e.message
            ),
        },
        _ => relay_error(e, Some(relay_session)),
    })?;
    let outcome = match action {
        NetAction::Status => {
            let st = c.status().map_err(|e| relay_error(e, Some(relay_session)))?;
            let row = st.sessions.iter().find(|s| s.name == relay_session).cloned();
            json!({
                "action": "status",
                "relay": {
                    "up": st.up,
                    "down_reason": st.down_reason,
                    "pasta_pid": st.pasta_pid,
                    "pasta_version": st.pasta_version,
                    "host_ports": st.host_ports,
                    "sessions": st.sessions.len(),
                },
                // `null` when this session has no relay netns: a session without a
                // row has no network, and the reply says so instead of implying one.
                "session": row,
                "networked": row.is_some(),
            })
        }
        NetAction::Offline(offline) => {
            let s = c
                .offline(relay_session, *offline)
                .map_err(|e| relay_error(e, Some(relay_session)))?;
            json!({"action": action.name(), "session": s})
        }
        NetAction::Delay { ms, jitter_ms, loss_pct } => {
            let s = c
                .delay(relay_session, *ms, *jitter_ms, *loss_pct)
                .map_err(|e| relay_error(e, Some(relay_session)))?;
            json!({"action": "delay", "session": s})
        }
        NetAction::HostPort { port, on } => {
            let st = c
                .host_port(*port, *on, Some(relay_session))
                .map_err(|e| relay_error(e, Some(relay_session)))?;
            json!({
                "action": "hostport",
                "port": port,
                "on": on,
                "host_ports": st.host_ports,
                "session": st.sessions.iter().find(|s| s.name == relay_session).cloned(),
            })
        }
    };
    Ok(outcome)
}

impl Module for NetworkModule {
    fn manifest(&self) -> &'static ModuleManifest {
        manifest_ref()
    }

    fn attach(
        &mut self,
        _host: &mut ServiceHost,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()> {
        if !manifest_ref().supports(profile) {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "profile {profile} is not implemented: this module has `off` (the default: no \
                     network at all) and `real` (the acos-netd relay). `online`/`offline`/`delay` \
                     are control ops on the real arm, not profiles"
                ),
            });
        }
        let session = config
            .string(CONFIG_SESSION)
            .map(str::to_string)
            .or_else(|| std::env::var(SESSION_ID_ENV).ok())
            .filter(|value| !value.is_empty());
        let Some(session) = session else {
            // Fail closed: a relay session is named by the session id and nothing
            // else (NET4), so a networked session without one would be a network
            // no client can address. Refusing at attach is the honest answer.
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "the `real` profile needs a session id to name its relay session \
                     (`acos-<id>`): set {SESSION_ID_ENV} (the engine publishes it) or pass \
                     `config: {{\"{CONFIG_SESSION}\": \"<id>\"}}`"
                ),
            });
        };
        self.relay_session = Some(crate::network::relay_session_name(&session));
        // The one gate the framework and the launcher read. Set here, before the
        // guest's command is built, so `--unshare-net` is dropped, the seccomp
        // program allows AF_INET, `dnsproxyd` is served and
        // `ConnectivityManager` describes a real interface.
        crate::network::set_profile(profile);
        self.profile = profile;
        // The name this module owns: registered inside the attach scope, so a
        // failed attach registers nothing and a detach restores `core`'s.
        let state = std::sync::Arc::new(std::sync::Mutex::new(
            crate::connectivity::ConnectivityState::new(),
        ));
        let factory_state = std::sync::Arc::clone(&state);
        _host.register(CONNECTIVITY_NAME, CONNECTIVITY_DESCRIPTOR, move |_wiring| {
            Box::new(crate::connectivity::ConnectivityService::new(std::sync::Arc::clone(
                &factory_state,
            ))) as Box<dyn crate::host::ServiceObject>
        });
        self.connectivity = Some(state);
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        crate::network::clear();
        self.profile = Profile::Off;
        self.relay_session = None;
        self.connectivity = None;
        Ok(())
    }

    fn control(&mut self, host: &mut ServiceHost, op: &str, args: &Value) -> Result<Value> {
        if self.profile != Profile::Real {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: "module is off".to_string(),
            });
        }
        let relay_session = self.session()?.to_string();
        // One parser for both spellings: the op is the action, the args are the
        // action's arguments. An unknown op or an argument that means nothing for
        // it is refused by `NetAction::from_args`, never ignored.
        let mut request = match args {
            Value::Object(map) => map.clone(),
            Value::Null => serde_json::Map::new(),
            other => {
                return Err(ServiceError::ModuleRejected {
                    module: MODULE_ID,
                    detail: format!(
                        "`network` control arguments must be an object (or null), got {other}"
                    ),
                })
            }
        };
        request.insert("action".to_string(), Value::String(op.to_string()));
        let action = NetAction::from_args(&Value::Object(request)).map_err(|error| {
            ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: error.message,
            }
        })?;
        let outcome = run_action(&action, &relay_session)?;
        // NET6: `offline`/`online` are the app-visible network state too, not just
        // a packet filter. After the relay accepted the change (it removed the
        // session's default route and destroyed its live sockets, inside the
        // session netns):
        //
        // * the resolver proxy this process serves answers `EAI_NODATA` while
        //   offline (Android's `UnknownHostException`), so a *new* lookup is
        //   bounded without pretending an interface exists;
        // * `ConnectivityManager` loses or regains its default network and every
        //   registered `NetworkCallback` gets `onLost` / `onAvailable` (queued on
        //   the host, which delivers them after this control request).
        if let NetAction::Offline(offline) = action {
            crate::network::set_offline(offline);
            if let Some(state) = &self.connectivity {
                let deliveries = {
                    let mut state = state.lock().unwrap();
                    state.set_default_network(!offline)
                };
                for delivery in deliveries {
                    host.queue_delivery(delivery);
                }
            }
        }
        Ok(outcome)
    }

    fn report(&self) -> ModuleReport {
        let mut report = ModuleReport::of(manifest_ref(), self.profile);
        if self.profile == Profile::Real {
            report.reason = Some(format!(
                "real internet through the acos-netd relay; relay session {}",
                self.relay_session.as_deref().unwrap_or("<none>")
            ));
        }
        report
    }
}
