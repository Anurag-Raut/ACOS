//! The one plan both sides of the wire read: addresses, the hardened `pasta`
//! argv, the relay's rule set and the per-session control rules.
//!
//! **One source of truth.** Every number here is either derived from
//! [`acos_services::connectivity`] (the constants `LinkProperties` reports to
//! apps) or is a netfilter/`tc` recipe measured by the NETSHARE prototype.
//! `plan::session(idx)` is the successor of the spike's `pasta_args()`: the
//! transport configures the interface the framework *says* the session has.

use acos_services::connectivity;

/// Name prefix of the relay end of every session veth (`acosv1`, `acosv2`, …).
///
/// All netfilter rules that talk about "any session" match `acosv+`, so a rule
/// can never accidentally match the relay's `lo` or `tap0`.
pub const VETH_PREFIX: &str = "acosv";

/// Name prefix of the session end of every veth pair (`acoss1`, …). It lives
/// inside the session's own netns.
pub const PEER_PREFIX: &str = "acoss";

/// The chain per-session control rules live in, inside the relay netns.
pub const SESSION_CHAIN: &str = "ACOS-SESS";

/// Destinations the guest must never reach (ADR-0030 §3): RFC1918, link-local,
/// CGNAT, and the host's own loopback range.
///
/// The rules live in the **relay** netns, not in the session's: the session
/// netns is owned by the guest's user namespace, so a rule there would be
/// policy the guest could flush. The relay netns is owned by the supervisor's
/// user namespace, which the guest never holds a capability over.
pub const LAN_DROP_CIDRS: &[&str] = &[
    "192.168.0.0/16",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "169.254.0.0/16",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "0.0.0.0/8",
];

/// pasta's interface name inside the relay netns.
pub const RELAY_IFACE: &str = "tap0";

/// Environment variable naming the `pasta` binary (a static build of the
/// pinned tag `2026_10_02.cba3570`, see `research/spike-net/artifacts/passt-build.txt`).
pub const PASTA_ENV: &str = "ACOS_NET_PASTA";

/// The tag the pinned binary reports; a mismatch is reported by `status`, not
/// silently accepted.
pub const PASTA_TAG: &str = "2026_10_02.cba3570";

/// Where the relay's own (host-namespace) side lives, per session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionNet {
    /// Index of the session (1-based): the address block is `10.163.<idx>.0/30`.
    pub idx: u32,
    /// Relay end of the veth (inside the relay netns).
    pub veth: String,
    /// Session end of the veth (inside the session's netns).
    pub peer: String,
    /// The address the guest is configured with (`LinkProperties` reports it).
    pub address: [u8; 4],
    pub prefix_len: u8,
    /// The relay end, the guest's default gateway.
    pub gateway: [u8; 4],
    /// The DNS server handed to the guest (netd/dnsproxyd answers it).
    pub dns: [u8; 4],
}

impl SessionNet {
    /// `10.163.<idx>.2/30`.
    #[must_use]
    pub fn cidr(&self) -> String {
        format!("{}/{}", ip(self.address), self.prefix_len)
    }

    /// `10.163.<idx>.1` — the relay end, written as a `/30` on the veth.
    #[must_use]
    pub fn gateway_cidr(&self) -> String {
        format!("{}/{}", ip(self.gateway), self.prefix_len)
    }
}

/// `a.b.c.d`.
#[must_use]
pub fn ip(a: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
}

/// The plan for session `idx` (1-based), or `None` when the index has no
/// address block ([`connectivity::NET_SESSION_MAX_INDEX`]). Both the relay and
/// the session shim are built from it, so the interface `LinkProperties`
/// describes is the interface the session actually has.
#[must_use]
pub fn session(idx: u32) -> Option<SessionNet> {
    let net = connectivity::session_network(idx)?;
    Some(SessionNet {
        idx,
        veth: format!("{VETH_PREFIX}{idx}"),
        peer: format!("{PEER_PREFIX}{idx}"),
        address: net.address,
        prefix_len: net.prefix_len,
        gateway: net.gateway,
        dns: connectivity::NET_DNS_SERVER,
    })
}

/// The hardened `pasta` argument list for the relay netns, up to and including
/// `--`.
///
/// The relay is the only NAT process on the host, so its flags carry the whole
/// boundary (ADR-0030 amendment 2):
///
/// * `-t none -u none -T none -U none` — every port-mirroring direction is off.
///   `-T/-U none` alone is not enough: `-t auto`/`-u auto` still scan the host's
///   loopback listeners in `--netns` mode (measured leak, SPIKE-NET attempt 3).
/// * `--no-splice` — no layer-4 splice between guest and host sockets.
/// * `--map-host-loopback none --no-map-gw` — the gateway address is not
///   rewritten to the host.
/// * `-a/-g/-D` come from [`acos_services::connectivity`], the same numbers
///   `LinkProperties` reports.
///
/// `ports` are the explicitly named host ports (Metro 8081). Each one becomes a
/// pasta forward, i.e. reachable from the relay; per-session access is gated by
/// [`hostport_gate_rule`].
#[must_use]
pub fn relay_pasta_args(ports: &[u16]) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--config-net".into(),
        "-I".into(),
        RELAY_IFACE.into(),
        "-a".into(),
        format!(
            "{}/{}",
            ip(connectivity::NET_GUEST_ADDRESS),
            connectivity::NET_GUEST_PREFIX_LEN
        ),
        "-g".into(),
        ip(connectivity::NET_GUEST_GATEWAY),
        "--no-map-gw".into(),
        "--map-host-loopback".into(),
        "none".into(),
        "-T".into(),
        "none".into(),
        "-U".into(),
        "none".into(),
        "--no-splice".into(),
        "-D".into(),
        ip(connectivity::NET_DNS_SERVER),
    ];
    if ports.is_empty() {
        args.push("-t".into());
        args.push("none".into());
        args.push("-u".into());
        args.push("none".into());
    } else {
        // A named host port only: `-t <port>` forwards guest connections to the
        // host's loopback listener of the same port, and nothing else.
        for p in ports {
            args.push("-t".into());
            args.push(p.to_string());
        }
        args.push("-u".into());
        args.push("none".into());
    }
    args.push("-f".into());
    args.push("-q".into());
    args.push("--".into());
    args
}

/// The relay-side rule set, one shell line each, installed in the relay netns
/// before any session can attach.
///
/// Order matters: return traffic for established flows is accepted before the
/// destination drops, and everything that is not explicitly allowed to leave
/// through `tap0` is dropped (`-i acosv+ -j DROP` at the end).
#[must_use]
pub fn relay_rules() -> Vec<String> {
    let mut rules = vec![
        "sysctl -qw net.ipv4.ip_forward=1".to_string(),
        format!("iptables -w -t nat -F POSTROUTING"),
        format!("iptables -w -t nat -A POSTROUTING -o {RELAY_IFACE} -j MASQUERADE"),
        "iptables -w -F FORWARD".to_string(),
        format!("(iptables -w -N {SESSION_CHAIN} 2>/dev/null; iptables -w -F {SESSION_CHAIN})"),
        format!("iptables -w -A FORWARD -j {SESSION_CHAIN}"),
        "iptables -w -A FORWARD -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT".to_string(),
    ];
    for cidr in LAN_DROP_CIDRS {
        rules.push(format!("iptables -w -A FORWARD -i {VETH_PREFIX}+ -d {cidr} -j DROP"));
    }
    rules.push(format!("iptables -w -A FORWARD -o {RELAY_IFACE} -j ACCEPT"));
    rules.push(format!("iptables -w -A FORWARD -i {VETH_PREFIX}+ -j DROP"));
    rules
}

/// The two lines that create one session's veth pair inside the relay netns and
/// move the session end into the session's netns.
#[must_use]
pub fn veth_attach_rules(net: &SessionNet, netns_pid: u32) -> Vec<String> {
    vec![
        format!(
            "ip link add {} type veth peer name {}",
            net.veth, net.peer
        ),
        format!(
            "ip link set {} netns /proc/{netns_pid}/ns/net",
            net.peer
        ),
        format!("ip addr add {} dev {}", net.gateway_cidr(), net.veth),
        format!("ip link set {} up", net.veth),
    ]
}

/// The veth is the whole per-session footprint: deleting the relay end deletes
/// the pair, including the end that lives in the session netns.
#[must_use]
pub fn veth_detach_rules(net: &SessionNet) -> Vec<String> {
    vec![format!("ip link del {}", net.veth)]
}

/// Offline: drop every packet the session sends (and, because the reply path
/// has no session to return to, everything it would have received).
#[must_use]
pub fn offline_rule(net: &SessionNet, on: bool) -> String {
    let flag = if on { "-I" } else { "-D" };
    format!("iptables -w {flag} {SESSION_CHAIN} -i {} -j DROP", net.veth)
}

/// Latency/loss: a `netem` qdisc on the session's veth inside the relay netns.
///
/// `netem` shapes the *relay* end of the session's veth, so the delay applies to
/// that session's traffic only and nothing in the guest participates.
#[must_use]
pub fn netem_rule(net: &SessionNet, delay_ms: u32, jitter_ms: u32, loss_pct: u32) -> String {
    let mut args = format!("tc qdisc replace dev {} root netem", net.veth);
    if delay_ms > 0 {
        args.push_str(&format!(" delay {delay_ms}ms"));
        if jitter_ms > 0 {
            args.push_str(&format!(" {jitter_ms}ms"));
        }
    }
    if loss_pct > 0 {
        args.push_str(&format!(" loss {loss_pct}%"));
    }
    args
}

/// Remove the shaping qdisc, restoring the session to the unshaped relay.
#[must_use]
pub fn netem_clear_rule(net: &SessionNet) -> String {
    format!("tc qdisc del dev {} root", net.veth)
}

/// Host-port opt-in: a session that has not asked for `port` must not reach it,
/// even when the relay forwards it for another session.
#[must_use]
pub fn hostport_gate_rule(net: &SessionNet, port: u16, allow: bool) -> String {
    if allow {
        format!("iptables -w -D {SESSION_CHAIN} -i {} -p tcp --dport {port} -j DROP", net.veth)
    } else {
        format!("iptables -w -I {SESSION_CHAIN} -i {} -p tcp --dport {port} -j DROP", net.veth)
    }
}

/// The session-side shim: run **inside the session netns**, root in the
/// supervisor's user namespace, before the guest exists.
///
/// It waits for the relay to move the veth peer in, configures the address,
/// prefix and default route from [`SessionNet`], creates the ready file (that is
/// the attach timestamp), then execs the guest. Failure exits
/// [`SHIM_FAIL_EXIT`] without starting the guest: a session that cannot be
/// configured is not started at all (fail closed).
#[must_use]
pub fn session_shim(net: &SessionNet, ready_file: &str) -> String {
    format!(
        r#"set -u
netns=$(readlink /proc/self/ns/net)
fail() {{ printf 'acos-net: session=failed idx={idx} reason=%s exit={fail}\n' "$1" >&2; exit {fail}; }}
mark() {{ printf 'acos-net: %s\n' "$*" >&2; }}
# `/proc/self/net/dev` is the calling process's netns, so the wait is a shell
# read and not one `ip` exec per poll (a `/sys/class/net` read would show the
# *host* netns: that mount's sysfs superblock belongs to it).
found=
i=0
while [ "$i" -lt 3000 ]; do
  while read -r line; do
    case "$line" in
      *"{peer}:"*) found=yes; break ;;
    esac
  done </proc/self/net/dev
  [ -n "$found" ] && break
  i=$((i + 1))
  sleep 0.002
done
[ -n "$found" ] || fail no-veth
# One exec configures the interface: link up, address, default route.
printf 'link set {peer} up\naddr add {cidr} dev {peer}\nroute add default via {gw}\n' \
  | ip -batch - >/dev/null || fail ip-batch
# The default route is verified, not assumed: a session that cannot route is a
# failed session, not a silent offline one.
routes=$(ip route show default) || fail no-default-route
case "$routes" in
  *"via {gw}"*) ;;
  *) fail no-default-route ;;
esac
printf '%s %s\n' "$netns" "$(date +%s%N)" > {ready}
mark "session={idx} netns=$netns iface={peer} addr={cidr} gw={gw}"
exec "$@"
"#,
        idx = net.idx,
        fail = SHIM_FAIL_EXIT,
        peer = net.peer,
        cidr = net.cidr(),
        gw = ip(net.gateway),
        ready = ready_file,
    )
}

/// Exit code of the session shim when the session netns cannot be configured.
pub const SHIM_FAIL_EXIT: i32 = 111;
