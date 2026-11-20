//! NET5: the `network` service module — the switch, the control path, and the
//! framework view of "off" versus "real".
//!
//! ADR-0030 gives the session a real network through the `acos-netd` relay; this
//! module is what makes it a *module*: `off` by default (the honest no-network
//! device ACOS has always presented), `real` when attached, and the `acos_net`
//! actions are its control ops.
//!
//! Two facts have to be proven here rather than in prose:
//!
//! 1. **Off is honest.** With the module off, `ConnectivityManager`'s
//!    parcelables describe a device with no interface: no address, no DNS, no
//!    route, no `VALIDATED` capability, and the module's controls refuse. Nothing
//!    is invented to look like a phone.
//! 2. **Attaching `real` changes exactly that** — and the change comes from the
//!    module, not from an environment variable.
//!
//! Everything that touches the process-wide gate runs in a **child process**
//! (`ACOS_NET5_CHILD`), like the conformance kit's quiet process: the gate is one
//! process-wide slot (one launcher = one guest), so a sibling test flipping it
//! would make the measurement noise. The child also gets a private
//! `ACOS_NETD_STATE`, so the fail-closed checks never talk to another agent's
//! relay.

use serde_json::json;

use acos_services::module::{AttachMode, Module, ModuleConfig, ModuleManifest, Profile};
use acos_services::modules::network::{NetworkModule, SESSION_ID_ENV};
use acos_services::network;

/// The child process's switch.
const CHILD_ENV: &str = "ACOS_NET5_CHILD";

/// The pin between the two spellings: the module's control ops are the
/// `acos_net` actions, and neither surface can grow a private copy.
#[test]
fn the_control_ops_are_the_acos_net_actions() {
    let manifest = NetworkModule::manifest_ref();
    let ops: Vec<&str> = manifest.controls.iter().map(|op| op.name).collect();
    let schema = acos_netd_ctl::net_ctl::net_ctl_schema();
    let actions = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("the schema's action enum")
        .iter()
        .map(|value| value.as_str().expect("action names are strings"))
        .collect::<Vec<_>>();
    assert_eq!(
        ops, actions,
        "`acos_service_ctl {{module: \"network\", op: <x>}}` and `acos_net {{action: <x>}}` must be \
         the same list: the module's controls ARE the verb's actions"
    );
    // And the args the module accepts for each action are the schema's keys for
    // it: an action that takes arguments must say so in the manifest.
    for op in manifest.controls.iter() {
        if op.args.contains('{') {
            let keys = acos_netd_ctl::net_ctl::action_keys(op.name);
            assert!(
                !keys.is_empty(),
                "{} is documented with arguments but owns no schema key",
                op.name
            );
        }
    }
}

/// The manifest says what the module is, and the parts that would be a lie are
/// absent: no `stub`/`scripted` profile, `at-launch` attach, a host tool.
#[test]
fn the_manifest_declares_the_real_arm_only() {
    let manifest = NetworkModule::manifest_ref();
    manifest.validate().expect("the manifest is valid");
    assert_eq!(manifest.id, "network");
    assert_eq!(
        manifest.profiles,
        &[Profile::Real],
        "no `stub`/`scripted` profile: a fake interface would be a fake success; determinism comes \
         from the real arm's online/offline/delay controls"
    );
    assert_eq!(
        manifest.attach,
        AttachMode::AtLaunch,
        "a running guest cannot be moved into a netns (its seccomp program and its netns are \
         fixed at exec), so the module is `at-launch`"
    );
    assert!(
        manifest.requires_host_tools.contains(&"pasta")
            && manifest.requires_host_tools.contains(&"acos-netd"),
        "the real arm needs the relay: {:?}",
        manifest.requires_host_tools
    );
    assert!(manifest.budget.ram_mib > 0 && manifest.budget.startup_ms > 0);
    // `off` is not a profile (it is the absence of the module). The name it owns
    // is `ConnectivityManager`'s, taken over from `core` — `core` answers it only
    // while the module is off, so a session without the module is exactly the
    // device ACOS presented before it existed.
    assert_eq!(
        manifest.services.iter().map(|s| s.name).collect::<Vec<_>>(),
        vec!["connectivity"]
    );
    assert_eq!(manifest.replaces, &["connectivity"]);
    assert!(!manifest.profiles.contains(&Profile::Off));
}

/// A detached module refuses every control: nothing fakes a network.
#[test]
fn a_detached_module_refuses_its_controls() {
    let mut module = NetworkModule::new();
    let mut host = acos_services::ServiceHost::new();
    for op in ["status", "offline", "online", "delay", "hostport"] {
        match module.control(&mut host, op, &json!({})) {
            Err(acos_services::ServiceError::ModuleRejected { detail, .. }) => {
                assert!(detail.contains("off"), "{op}: {detail}");
            }
            other => panic!("{op} on a detached module answered {other:?}"),
        }
    }
    assert_eq!(module.report().profile, None);
    assert!(module.report().reason.expect("a reason").contains("off"));
}

/// The child: everything that needs the process-wide gate.
#[test]
fn the_framework_view_is_off_until_the_module_attaches() {
    let exe = std::env::current_exe().expect("test executable");
    let state = std::env::temp_dir().join(format!("acos-net5-state-{}", std::process::id()));
    let output = std::process::Command::new(exe)
        .args([
            "--exact",
            "child_framework_view",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        // No relay supervisor here: the fail-closed checks are deterministic.
        .env("ACOS_NETD_STATE", &state)
        .env_remove(SESSION_ID_ENV)
        .env_remove("ACOS_SPIKE_NET")
        .output()
        .expect("spawn the child");
    let _ = std::fs::remove_dir_all(&state);
    assert!(
        output.status.success(),
        "the child failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The child half (a no-op in the parent process).
#[test]
fn child_framework_view() {
    if std::env::var(CHILD_ENV).is_err() {
        return;
    }
    use acos_services::connectivity::{
        default_capabilities_bits, default_link_properties, CAP_VALIDATED,
    };
    use acos_services::value::{Parcelable, Value};

    /// The `LinkProperties` address list, straight out of the parcelable the
    /// framework reads.
    fn link_addresses() -> Vec<Value> {
        let Parcelable::Ordered { values, .. } = default_link_properties() else {
            panic!("LinkProperties is an ordered parcelable");
        };
        match &values[1] {
            Value::List(Some(list)) => list.clone(),
            other => panic!("LinkProperties link addresses are a list field, got {other:?}"),
        }
    }

    let mut host = acos_services::ServiceHost::new();
    let mut module = NetworkModule::new();
    let manifest = NetworkModule::manifest_ref();

    // -- off ---------------------------------------------------------------
    assert!(!network::session_has_network(), "off is the default state");
    let off_caps = default_capabilities_bits();
    assert_eq!(
        off_caps & (1i64 << CAP_VALIDATED),
        0,
        "no VALIDATED network"
    );
    assert!(
        link_addresses().is_empty(),
        "an off session has no address, ever"
    );

    // -- a profile the module does not implement is refused ---------------
    for profile in [Profile::Stub, Profile::Scripted] {
        let error = module
            .attach(
                &mut host,
                profile,
                &ModuleConfig::from_json("network", &json!({})).unwrap(),
            )
            .expect_err("no stub/scripted profile");
        assert!(
            error.to_string().contains("not implemented"),
            "{profile}: {error}"
        );
    }
    assert!(
        !network::session_has_network(),
        "a refused attach changed nothing"
    );

    // -- `real` without a session id fails closed --------------------------
    let error = module
        .attach(
            &mut host,
            Profile::Real,
            &ModuleConfig::from_json("network", &json!({})).unwrap(),
        )
        .expect_err("a networked session needs an id");
    assert!(
        error.to_string().contains(SESSION_ID_ENV),
        "the refusal names the missing id: {error}"
    );
    assert!(!network::session_has_network());

    // -- `real` with one: the gate, the report, the framework view --------
    module
        .attach(
            &mut host,
            Profile::Real,
            &ModuleConfig::from_json("network", &json!({"session": "net5-child"})).unwrap(),
        )
        .expect("attach");
    assert!(network::session_has_network(), "the module is the gate");
    assert_eq!(module.relay_session(), Some("acos-net5-child"));
    let report = module.report();
    assert_eq!(report.profile, Some(Profile::Real));
    assert!(report.attached);
    assert_eq!(report.id, manifest.id);
    assert!(report
        .reason
        .as_deref()
        .unwrap_or_default()
        .contains("acos-net5-child"));

    let on_caps = default_capabilities_bits();
    assert_ne!(
        on_caps & (1i64 << CAP_VALIDATED),
        0,
        "an attached session reports a validated internet network"
    );
    assert_eq!(
        link_addresses().len(),
        1,
        "the interface the namespace really has is described, not invented"
    );

    // -- the controls run on the module (and are the relay's) --------------
    let err = module
        .control(&mut host, "no-such-op", &json!({}))
        .expect_err("unknown op");
    assert!(
        err.to_string().contains("unknown action") || err.to_string().contains("no-such-op"),
        "{err}"
    );
    let err = module
        .control(&mut host, "delay", &json!({"port": 9}))
        .expect_err("a key that means nothing for the action");
    assert!(
        err.to_string().contains("means nothing for action `delay`"),
        "{err}"
    );
    // No supervisor is running under this state dir: `status` must say so with
    // `NETWORK_UNAVAILABLE`, never a silent offline.
    let err = module
        .control(&mut host, "status", &json!(null))
        .expect_err("no relay");
    assert_eq!(
        err.code(),
        acos_services::error::codes::NETWORK_UNAVAILABLE,
        "{err}"
    );
    assert!(
        err.to_string().contains("acos-netd up"),
        "the failure names the fix: {err}"
    );

    // -- detach puts the framework back where it started -------------------
    module.detach(&mut host).expect("detach");
    assert!(!network::session_has_network());
    assert_eq!(
        default_capabilities_bits() & (1i64 << CAP_VALIDATED),
        0,
        "a detached module leaves the honest no-network device"
    );
    assert!(link_addresses().is_empty());
}
