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
///
/// One definition: `acos_netd_ctl::pasta` owns the name, the tag and the search
/// order (`acos doctor`, this supervisor, the scenario runners and the installer
/// all read it).
pub use acos_netd_ctl::pasta::ENV as PASTA_ENV;

/// The tag the pinned binary reports; a mismatch is reported by `status`, not
/// silently accepted.
pub use acos_netd_ctl::pasta::TAG as PASTA_TAG;

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

/// The address pasta rewrites to the host's `127.0.0.1` (and back), and therefore
/// the address a session names when it wants a host loopback service.
///
/// It is the relay's own gateway ([`acos_services::connectivity::NET_GUEST_GATEWAY`]),
/// the one address every session already routes to: the relay forwards it out of
/// `tap0`, and pasta's inbound NAT turns `MAP:PORT` into `127.0.0.1:PORT` on the
/// host. Nothing else on the host's loopback is reachable: every session gets a
/// default-deny rejection of this address at attach ([`hostport_default_rules`])
/// and a named port is opened only by an explicit opt-in
/// ([`hostport_allow_rule`]).
#[must_use]
pub fn host_loopback_map() -> String {
    ip(connectivity::NET_GUEST_GATEWAY)
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
///   `-T <port>` is **not** a way to open a host port for a relay session: on the
///   pinned tag a `-T` rule is served by a listening socket that pasta binds
///   inside *its own* target namespace (the relay netns) and only for
///   loopback → loopback flows — `fwd_nat_from_splice()` refuses anything else
///   with `PIF_NONE` (`fwd.c`), and a session cannot reach the relay netns'
///   `127.0.0.1` at all. NET2 measured the consequence; NET3 §host-port measures
///   every `-T` form of the pinned tag and uses [`host_loopback_map`] instead.
/// * `--map-host-loopback <MAP>` — the *only* host-loopback path there is: pasta
///   rewrites the relay's `MAP` destination to the host's `127.0.0.1`. It is
///   gated per session per port in the relay netns (see [`hostport_default_rules`]
///   and [`hostport_allow_rule`]); `--no-map-gw` keeps pasta from choosing that
///   address by itself.
/// * `--no-splice` — no layer-4 splice between guest and host sockets.
/// * `-a/-g/-D` come from [`acos_services::connectivity`], the same numbers
///   `LinkProperties` reports.
#[must_use]
pub fn relay_pasta_args() -> Vec<String> {
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
        host_loopback_map(),
        "--no-splice".into(),
        "-D".into(),
        ip(connectivity::NET_DNS_SERVER),
    ];
    // Every direction starts closed, and stays closed: host ports are opened by
    // the per-session gate below, not by a pasta forward.
    for flag in ["-t", "-u", "-T", "-U"] {
        args.push(flag.into());
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
        rules.push(format!(
            "iptables -w -A FORWARD -i {VETH_PREFIX}+ -d {cidr} -j DROP"
        ));
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
        format!("ip link add {} type veth peer name {}", net.veth, net.peer),
        format!("ip link set {} netns /proc/{netns_pid}/ns/net", net.peer),
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
///
/// NET5 open finding (`results/handoff/NET5/FINDING-OFFLINE.md`) measured that a
/// relay-side drop alone is *not* an "offline" a guest can observe: after
/// `offline`, the app's second `HttpURLConnection` fetch reused the first
/// fetch's keep-alive TLS socket and blocked in `read(2)` forever — 60 s, no
/// exception, no resolver request — because nothing told the guest kernel or the
/// app that the network was gone. NET6 measured the exact frame
/// (`SocketInputStream.socketRead0` under `Http1xStream.readResponseHeaders`)
/// and fixed it the way Android does (see [`session_offline_cmds`]).
///
/// This rule stays: it is the fail-closed packet-level half of `offline`, and it
/// also covers packets already in flight. [`session_offline_cmds`] below is the
/// guest-facing half.
#[must_use]
pub fn offline_rule(net: &SessionNet, on: bool) -> String {
    let flag = if on { "-I" } else { "-D" };
    format!("iptables -w {flag} {SESSION_CHAIN} -i {} -j DROP", net.veth)
}

/// The **guest-facing** half of `offline`, run inside the session's own netns.
///
/// A phone that loses its network does two things an app can feel, and ACOS now
/// does both:
///
/// * the default network goes away, so every new `connect(2)` fails at once with
///   `ENETUNREACH` (Android: `java.net.SocketException: Network is unreachable`)
///   instead of sitting in SYN retries — measured here: removing the default
///   route makes `connect("1.1.1.1", 80)` fail in 0 ms with errno 101, while the
///   route present makes it hang for the whole TCP budget;
/// * sockets already open on the lost network are **destroyed** (`ss -K`, the
///   userspace form of netd's `SOCK_DESTROY`), so a blocked read returns
///   `ECONNRESET`/`ECONNABORTED` immediately — Android: `SocketException:
///   Connection reset`. This is the half the NET5 finding needed: the stuck
///   fetch was a *reused keep-alive* socket, so no route change alone can free
///   it.
///
/// The interface itself is left up: the address and the veth are the session's
/// identity, and a route is what "has a default network" means. Bringing the
/// link down would flush the address too, and `online` would have to rebuild it
/// instead of restoring one route.
#[must_use]
pub fn session_offline_cmds(net: &SessionNet, netns_pid: u32) -> Vec<String> {
    // `ss -K` has no filter for "every state", so the states a live session can
    // hold are named; a `time-wait` socket has no reader to wake.
    let kill = format!(
        "nsenter -t {netns_pid} -n -- sh -c 'for st in established syn-sent close-wait; do \
         ss -K state $st >/dev/null 2>&1; done; true'"
    );
    vec![
        format!("nsenter -t {netns_pid} -n -- sh -c 'ip route del default 2>/dev/null; true'"),
        kill,
        // Recorded for the report: the session really has no default route now.
        format!("nsenter -t {netns_pid} -n -- sh -c 'ip route show default'"),
    ]
}

/// The `online` half: restore exactly the route [`session_offline_cmds`]
/// removed (`replace` is idempotent, so a doubled `online` is a no-op), then the
/// relay's drop rule is taken out by the caller.
#[must_use]
pub fn session_online_cmds(net: &SessionNet, netns_pid: u32) -> Vec<String> {
    vec![format!(
        "nsenter -t {netns_pid} -n -- ip route replace default via {}",
        ip(net.gateway)
    )]
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

/// Host-port opt-in, part 1: **every** session starts with the host's loopback
/// unreachable. The rule is appended at attach, so it is below any later
/// [`hostport_allow_rule`] insert (netfilter evaluates a chain top to bottom, and
/// an `ACCEPT` in this chain ends the packet's traversal — it never reaches the
/// relay's own link-local/CGNAT drops below it).
/// A `REJECT` with a TCP reset, not a `DROP`: a session that asks for a host
/// port it did not opt in to must be *refused* — an immediate RST, the answer a
/// phone gives for a closed port — not left to hang in SYN retries. Measured
/// (NET3): with a `DROP` the guest's own boot probe to the map address stalls
/// ~4.5 s; with a reset it fails at once, and "refused" is what the semantics
/// of an un-opened port promise.
#[must_use]
pub fn hostport_default_rules(net: &SessionNet) -> Vec<String> {
    vec![
        format!(
            "iptables -w -A {SESSION_CHAIN} -i {} -d {}/32 -p tcp -j REJECT --reject-with tcp-reset",
            net.veth,
            host_loopback_map()
        ),
        format!(
            "iptables -w -A {SESSION_CHAIN} -i {} -d {}/32 -j REJECT",
            net.veth,
            host_loopback_map()
        ),
    ]
}

/// Host-port opt-in, part 2: open exactly one port of the host's loopback for
/// exactly one session (`on = false` closes it again).
#[must_use]
pub fn hostport_allow_rule(net: &SessionNet, port: u16, on: bool) -> String {
    let flag = if on { "-I" } else { "-D" };
    format!(
        "iptables -w {flag} {SESSION_CHAIN} -i {} -d {}/32 -p tcp --dport {port} -j ACCEPT",
        net.veth,
        host_loopback_map()
    )
}

/// The per-session forwarder that binds `127.0.0.1:<port>` **inside the session
/// netns** and dials the relay's host-loopback map address.
///
/// This is what makes an opt-in host port usable by an app that does what an
/// Android app does: connect to `localhost` (`Metro`'s `ws://localhost:8081`, an
/// `adb reverse` target). The guest's own `lo` cannot be made to reach the host,
/// so the forwarder terminates the loopback connection in the session netns and
/// opens the gated path out. It is opt-in only: no forwarder, no listener, so a
/// connection to `127.0.0.1:<port>` is refused exactly as on a phone without
/// `adb reverse`.
#[must_use]
pub fn forwarder_args(port: u16, target: &str) -> Vec<String> {
    vec![
        "forward".to_string(),
        "--listen".to_string(),
        format!("127.0.0.1:{port}"),
        "--target".to_string(),
        target.to_string(),
    ]
}

/// The default dial target of the forwarder for a named port.
#[must_use]
pub fn forwarder_target(port: u16) -> String {
    format!("{}:{port}", host_loopback_map())
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
printf 'link set lo up\nlink set {peer} up\naddr add {cidr} dev {peer}\nroute add default via {gw}\n' \
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
