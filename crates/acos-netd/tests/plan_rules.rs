//! The transport's contract, asserted without a network: the hardened `pasta`
//! argv, the relay rule set, the per-session controls, the session shim, and the
//! one source of truth the shim shares with `LinkProperties`.
//!
//! The live half (a real relay netns, real sessions, real HTTP) is driven by
//! `results/handoff/NET1/proof.sh`; these tests are what the bench and the
//! module framework can run without a privileged kernel feature.

use acos_netd::plan::{self, SessionNet};
use acos_netd::proto::{self, ErrorCode, Request};
use acos_services::connectivity;

fn net(idx: u32) -> SessionNet {
    plan::session(idx).expect("a session index with an address block")
}

fn pair(args: &[String], flag: &str) -> String {
    let at = args
        .iter()
        .position(|a| a == flag)
        .unwrap_or_else(|| panic!("{flag} missing from {args:?}"));
    args[at + 1].clone()
}

/// ADR-0030 amendment 2 + 3: every port-mirroring direction off, no splice, the
/// host loopback reachable only through the *gated* map address, and the
/// addresses `LinkProperties` reports.
#[test]
fn relay_pasta_args_keep_the_host_boundary_and_match_link_properties() {
    let args = plan::relay_pasta_args();
    assert!(args.iter().any(|a| a == "--no-splice"), "no splice: {args:?}");
    assert!(args.iter().any(|a| a == "--no-map-gw"), "no gateway mapping by itself");
    // The map address is the relay's own gateway, and it is the *only* way to
    // the host's loopback: NET3 measured that a `-T` forward cannot serve a
    // relay session (see the plan's doc comment), so the map is the path and the
    // per-session gate is what makes it safe.
    assert_eq!(pair(&args, "--map-host-loopback"), plan::host_loopback_map());
    assert_eq!(plan::host_loopback_map(), plan::ip(connectivity::NET_GUEST_GATEWAY));
    assert_eq!(pair(&args, "-t"), "none", "host ports are never auto-forwarded");
    assert_eq!(pair(&args, "-u"), "none");
    assert_eq!(pair(&args, "-T"), "none");
    assert_eq!(pair(&args, "-U"), "none");
    // The interface the transport configures is the interface the framework
    // reports: same constants, not a copy.
    assert_eq!(
        pair(&args, "-a"),
        format!(
            "{}/{}",
            plan::ip(connectivity::NET_GUEST_ADDRESS),
            connectivity::NET_GUEST_PREFIX_LEN
        )
    );
    assert_eq!(pair(&args, "-g"), plan::ip(connectivity::NET_GUEST_GATEWAY));
    assert_eq!(pair(&args, "-D"), plan::ip(connectivity::NET_DNS_SERVER));
    assert_eq!(args.last().map(String::as_str), Some("--"), "argv ends at --");
}

/// No host port is a pasta forward, ever.
///
/// The pinned tag's `-T` rule is served by a listening socket pasta binds in its
/// own target namespace and only for loopback → loopback flows
/// (`fwd_nat_from_splice` returns `PIF_NONE` otherwise), so no `-T` form can open
/// a host port for a session in a *different* netns. Host ports are the gated map
/// address instead; the relay's argv must never grow a forward again.
#[test]
fn relay_pasta_args_never_forward_a_host_port() {
    let args = plan::relay_pasta_args();
    for flag in ["-t", "-u", "-T", "-U"] {
        assert_eq!(pair(&args, flag), "none", "{flag} must be `none`: {args:?}");
    }
    assert!(
        !args.iter().any(|a| a.chars().all(|c| c.is_ascii_digit())),
        "no bare port in the relay's argv: {args:?}"
    );
}

/// The LAN boundary is installed in the relay netns, in the right order, and it
/// covers every range ADR-0030 §3 names plus the host's own loopback.
#[test]
fn relay_rules_drop_lan_link_local_cgnat_and_host_loopback() {
    let rules = plan::relay_rules();
    for cidr in ["192.168.0.0/16", "10.0.0.0/8", "172.16.0.0/12", "169.254.0.0/16", "100.64.0.0/10", "127.0.0.0/8"] {
        assert!(
            rules.iter().any(|r| r.contains(&format!("-i acosv+ -d {cidr} -j DROP"))),
            "{cidr} must be dropped for session traffic: {rules:?}"
        );
    }
    let joined = rules.join("\n");
    // Return traffic is accepted before the destination drops, or a NATed TCP
    // connection could never come back.
    let established = joined.find("ESTABLISHED,RELATED").expect("conntrack rule");
    let first_drop = joined.find("-d 192.168.0.0/16").expect("a drop rule");
    assert!(established < first_drop, "conntrack accept must precede the drops");
    // Everything that is not explicitly allowed out of tap0 is dropped.
    assert!(rules.iter().any(|r| r == "iptables -w -A FORWARD -i acosv+ -j DROP"));
    assert!(rules.iter().any(|r| r.contains("iptables -w -A FORWARD -o tap0 -j ACCEPT")));
    assert!(rules.iter().any(|r| r.contains("MASQUERADE")));
}

/// Each session's addresses come from the connectivity module, so what apps read
/// in `LinkProperties` is what the session netns actually has.
#[test]
fn session_plan_is_link_properties_per_session() {
    for idx in [1u32, 5, 20, 254] {
        let n = net(idx);
        let expected = connectivity::session_network(idx).expect("in range");
        assert_eq!(n.address, expected.address);
        assert_eq!(n.gateway, expected.gateway);
        assert_eq!(n.prefix_len, connectivity::NET_SESSION_PREFIX_LEN);
        assert_eq!(n.cidr(), format!("10.163.{idx}.2/30"));
        assert_eq!(n.gateway_cidr(), format!("10.163.{idx}.1/30"));
        assert_eq!(n.veth, format!("acosv{idx}"));
        assert_eq!(n.peer, format!("acoss{idx}"));
        assert_eq!(n.dns, connectivity::NET_DNS_SERVER);
    }
    // No silent reuse of an address block.
    assert!(plan::session(0).is_none());
    assert!(plan::session(255).is_none());
    assert!(connectivity::session_network(0).is_none());
}

/// Two sessions never share an address or an interface name.
#[test]
fn sessions_are_distinct() {
    let a = net(1);
    let b = net(2);
    assert_ne!(a.address, b.address);
    assert_ne!(a.veth, b.veth);
    assert_ne!(a.peer, b.peer);
}

/// The veth pair is the session's whole footprint: it is created in the relay
/// netns and its peer is moved into the session's own netns.
#[test]
fn veth_attach_moves_the_peer_into_the_session_netns() {
    let n = net(7);
    let rules = plan::veth_attach_rules(&n, 4242);
    assert_eq!(rules[0], "ip link add acosv7 type veth peer name acoss7");
    assert_eq!(rules[1], "ip link set acoss7 netns /proc/4242/ns/net");
    assert_eq!(rules[2], "ip addr add 10.163.7.1/30 dev acosv7");
    assert_eq!(rules[3], "ip link set acosv7 up");
    assert_eq!(plan::veth_detach_rules(&n), vec!["ip link del acosv7".to_string()]);
}

/// Per-session controls touch one veth and nothing else.
#[test]
fn controls_are_per_session() {
    let a = net(1);
    let b = net(2);
    let off = plan::offline_rule(&a, true);
    assert!(off.contains("-I ACOS-SESS -i acosv1 -j DROP"), "{off}");
    assert!(!off.contains("acosv2"));
    let on = plan::offline_rule(&a, false);
    assert!(on.contains("-D ACOS-SESS -i acosv1 -j DROP"), "{on}");

    let shaped = plan::netem_rule(&b, 400, 20, 5);
    assert!(shaped.contains("dev acosv2 root netem delay 400ms 20ms loss 5%"), "{shaped}");
    assert!(plan::netem_rule(&b, 0, 0, 0).contains("dev acosv2 root netem"));
    assert_eq!(plan::netem_clear_rule(&b), "tc qdisc del dev acosv2 root");

    // Host ports: default deny the whole host loopback at attach, then one
    // ACCEPT per opted-in port, inserted above that DROP.
    let deny = plan::hostport_default_rule(&b);
    assert_eq!(
        deny,
        format!(
            "iptables -w -A ACOS-SESS -i acosv2 -d {}/32 -j DROP",
            plan::host_loopback_map()
        ),
        "{deny}"
    );
    let allow = plan::hostport_allow_rule(&b, 8081, true);
    assert_eq!(
        allow,
        format!(
            "iptables -w -I ACOS-SESS -i acosv2 -d {}/32 -p tcp --dport 8081 -j ACCEPT",
            plan::host_loopback_map()
        ),
        "{allow}"
    );
    assert!(plan::hostport_allow_rule(&b, 8081, false).starts_with("iptables -w -D ACOS-SESS"));
    // The forwarder's two ends: the session's loopback, and the gated map address.
    assert_eq!(
        plan::forwarder_args(8081, &plan::forwarder_target(8081)),
        vec![
            "forward".to_string(),
            "--listen".to_string(),
            "127.0.0.1:8081".to_string(),
            "--target".to_string(),
            format!("{}:8081", plan::host_loopback_map()),
        ]
    );
}

/// The shim configures the session from the plan, records the attach timestamp
/// only once the default route exists, and fails closed before the guest runs.
#[test]
fn session_shim_configures_then_execs_and_fails_closed() {
    let n = net(3);
    let shim = plan::session_shim(&n, "/run/x/ready-3.txt");
    assert!(shim.contains("/proc/self/net/dev"), "waits for the veth in its own netns, without an exec per poll");
    assert!(shim.contains("addr add 10.163.3.2/30 dev acoss3"), "one `ip -batch` exec, not four: {shim}");
    assert!(shim.contains("link set lo up"), "the session's own loopback is up (a forwarder binds it): {shim}");
    assert!(shim.contains("route add default via 10.163.3.1"));
    assert!(shim.contains("via 10.163.3.1"), "the default route is verified, not assumed");
    assert!(shim.contains("/run/x/ready-3.txt"));
    let ready = shim.find("ready-3.txt").expect("ready file");
    let exec = shim.find("exec \"$@\"").expect("the guest runs");
    assert!(ready < exec, "the timestamp is written before the guest starts");
    // Every failure path exits before the guest can run.
    assert!(shim.rfind("|| fail") .expect("a fail path") < exec);
    assert!(shim.contains(&format!("exit {}", plan::SHIM_FAIL_EXIT)));
    assert!(shim.contains("no-veth") && shim.contains("no-default-route"));
}

/// The wire format round-trips, and a failure code is explicit, not a silent
/// success.
#[test]
fn protocol_round_trips_and_fails_closed() {
    let req = Request::Offline { session: "joplin".into(), offline: true };
    let line = proto::encode(&req);
    assert!(line.ends_with('\n'));
    match proto::decode_request(&line).expect("a request") {
        Request::Offline { session, offline } => {
            assert_eq!(session, "joplin");
            assert!(offline);
        }
        other => panic!("wrong request: {other:?}"),
    }
    let reply = proto::Reply::fail(ErrorCode::NetworkUnavailable, "pasta died");
    let json = serde_json::to_value(&reply).expect("json");
    assert_eq!(json["ok"], serde_json::json!(false));
    assert_eq!(json["error"]["code"], serde_json::json!("network-unavailable"));
    assert!(proto::decode_request("{\"op\":\"nope\"}").is_err());
}

/// `ACOS_NET_ARM` selects the transport, and a typo is an error rather than a
/// silent switch.
#[test]
fn net_arm_is_explicit() {
    let prev = std::env::var_os(acos_netd::client::ARM_ENV);
    std::env::set_var(acos_netd::client::ARM_ENV, "base");
    assert_eq!(acos_netd::client::Arm::from_env().expect("base"), acos_netd::client::Arm::Base);
    std::env::set_var(acos_netd::client::ARM_ENV, "relay");
    assert_eq!(acos_netd::client::Arm::from_env().expect("relay"), acos_netd::client::Arm::Relay);
    std::env::set_var(acos_netd::client::ARM_ENV, "railly");
    assert!(acos_netd::client::Arm::from_env().is_err(), "a typo must not pick an arm");
    match prev {
        Some(v) => std::env::set_var(acos_netd::client::ARM_ENV, v),
        None => std::env::remove_var(acos_netd::client::ARM_ENV),
    }
}
