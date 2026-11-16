//! SPIKE-NET — real network through `pasta` (spike gate for ADR-0030).
//!
//! **This module is spike code, not product code.** Everything in it is gated
//! behind `ACOS_SPIKE_NET=1`; with the variable unset the process behaves
//! exactly as before (session netns unshared, no pasta, no DNS proxy, seccomp
//! allows only `AF_UNIX`).
//!
//! What the spike proves (see `research/spike-net/LOG.md` for the transcript):
//!
//! * `pasta` attaches the session to a NATed network without root or a system
//!   package: the sandbox keeps every other namespace, drops `--unshare-net`
//!   and is executed *inside* the netns pasta created for it.
//! * Host loopback and the LAN stay unreachable: pasta is started with
//!   `-T none -U none --no-splice` (no host→namespace port forwarding and no
//!   loopback splice; the defaults forward every port the host has bound on
//!   loopback, which is exactly the leak ADR-0030 forbids), `--no-map-gw` and
//!   `--map-host-loopback none`, plus in-namespace `iptables` DROP rules for
//!   RFC1918 destinations (the namespace is owned by pasta's user namespace,
//!   so the guest side has `CAP_NET_ADMIN` over the netns and nothing else).
//! * Android's bionic resolver reaches `/dev/socket/dnsproxyd`; a minimal,
//!   netd-compatible responder on the host side of the session answers it, so
//!   `getaddrinfo`/`gethostbyname` work in the guest without netd.
//!
//! Deliberate spike limits, so nothing here is mistaken for the finished
//! feature: `gethostbyname` is not implemented (bionic routes both through
//! `getaddrinfo`), there is no netd `DnsResolver` binder, no latency/loss
//! controls, no HAR proxy, and `pasta` is looked up from a path in
//! `ACOS_SPIKE_NET_PASTA` (the installer, packaging and `acos doctor` checks
//! are out of scope for the spike).

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The spike's environment override (`1` = on, unset = off). The **product**
/// gate is the `network` service module (ADR-0029/ADR-0030): attaching it
/// publishes the profile in [`acos_services::network`], which is what
/// [`enabled`] asks. The variable only remains for the standalone `acos-art`
/// binary's own spike scripts and boundary tests, which have no module registry.
pub const FLAG: &str = "ACOS_SPIKE_NET";
/// Path of the `pasta` binary the spike uses (default: `pasta` in `PATH`).
pub const PASTA_ENV: &str = "ACOS_SPIKE_NET_PASTA";
/// Optional file the DNS responder appends one line per request to (spike
/// debugging: it answers the question "was the proxy contacted at all, and with
/// what?"). Unset in normal runs.
pub const DNS_LOG_ENV: &str = "ACOS_SPIKE_NET_DNS_LOG";

/// True when this session has a real network. The only gate in the module.
///
/// NET5: one gate for every consumer — the `network` service module sets it at
/// attach (before the guest's command is built), so `--unshare-net`, the seccomp
/// program, `dnsproxyd` and `ConnectivityManager` all answer the same question
/// the same way. `ACOS_SPIKE_NET` is the standalone launcher's override.
#[must_use]
pub fn enabled() -> bool {
    acos_services::network::session_has_network()
}

/// Flags of the sandbox's bwrap command.
///
/// The production list is [`crate::SANDBOX_FLAGS`]; the spike drops exactly one
/// entry — `--unshare-net` — because the namespace now comes from pasta. Every
/// other namespace, `--die-with-parent`, `--clearenv` and `--cap-drop ALL`
/// stay, so the sandbox is not weaker apart from the network it is meant to
/// have.
#[must_use]
pub fn sandbox_flags() -> Vec<&'static str> {
    if !enabled() {
        return crate::SANDBOX_FLAGS.to_vec();
    }
    crate::SANDBOX_FLAGS.iter().copied().filter(|f| *f != "--unshare-net").collect()
}

/// The hardened `pasta` argument list, up to and including `--`.
///
/// Verified from inside the namespace by `research/spike-net/probe.c`:
/// `internet=reachable host_loopback=blocked lan=blocked dns=ok`.
///
/// * `-T none -U none --no-splice` — pasta's defaults (`auto`) mirror every
///   host port bound on loopback into the namespace and splice local traffic
///   between the two layer-4 stacks; that made `127.0.0.1:<host port>`
///   reachable from the guest in the first attempt (LOG.md, attempt 3).
/// * `--map-host-loopback none` (implies `--no-map-gw`) — the gateway address
///   is not rewritten to the host, so the guest cannot reach host services by
///   aiming at its default gateway.
/// * `-a <guest>/24 -g <gateway>` — the guest gets a link-local address, not
///   the host's LAN address, so nothing in the guest looks like it is on the LAN.
/// * `-D <dns>` — the DHCP/NDP DNS answer handed to the guest.
///
/// The three addresses come from [`acos_services::connectivity`], which is also
/// what `LinkProperties` reports to apps: the interface an app reads and the
/// interface pasta configures are the same numbers by construction.
#[must_use]
pub fn pasta_args() -> Vec<String> {
    use acos_services::connectivity::{
        NET_DNS_SERVER, NET_GUEST_ADDRESS, NET_GUEST_GATEWAY, NET_GUEST_PREFIX_LEN,
    };
    let ip = |a: [u8; 4]| format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]);
    [
        "-f".to_string(),
        "-q".to_string(),
        "--config-net".to_string(),
        "-a".to_string(),
        format!("{}/{}", ip(NET_GUEST_ADDRESS), NET_GUEST_PREFIX_LEN),
        "-g".to_string(),
        ip(NET_GUEST_GATEWAY),
        "--no-map-gw".to_string(),
        "--map-host-loopback".to_string(),
        "none".to_string(),
        "-T".to_string(),
        "none".to_string(),
        "-U".to_string(),
        "none".to_string(),
        "--no-splice".to_string(),
        "-D".to_string(),
        ip(NET_DNS_SERVER),
        "--".to_string(),
    ]
    .into_iter()
    .collect()
}

// ---------------------------------------------------------------------------
// The LAN boundary
// ---------------------------------------------------------------------------

/// The chain the drop rules live in, inside the session's own netns.
pub const BOUNDARY_CHAIN: &str = "acos-net-drop";

/// Marker prefix of every line the boundary shim writes (stderr, and the file
/// named by [`BOUNDARY_ENV`] when that is set).
pub const BOUNDARY_MARK: &str = "acos-net:";

/// Environment variable naming a file that the shim also appends its markers
/// to. Inherited by the shim from the launcher (the guest never sees it: bwrap
/// runs with `--clearenv`).
pub const BOUNDARY_ENV: &str = "ACOS_SPIKE_NET_BOUNDARY";

/// Exit code the shim uses when the boundary cannot be installed: the guest is
/// then **not** started at all (fail closed).
pub const BOUNDARY_FAIL_EXIT: i32 = 111;

/// Destination ranges that must never be reachable from the guest (ADR-0030
/// §3): RFC1918, link-local, and the CGNAT range.
pub const LAN_DROP_CIDRS: &[&str] = &[
    "192.168.0.0/16",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "169.254.0.0/16",
    "100.64.0.0/10",
];

/// The setup shim run inside the session netns, before bwrap (and therefore
/// before any guest code).
///
/// Why a shim and why here: `pasta` forwards *any* destination the guest sends
/// it, the LAN included (measured: `192.168.31.1:80 -> OK` with the hardened
/// flags). passt has no destination ACL, so the block is netfilter and it has
/// to be installed by the only process with `CAP_NET_ADMIN` over the new netns
/// — the command pasta spawns, which runs as root *in pasta's user
/// namespace*. bwrap then puts the guest in a **child** user namespace with
/// `--cap-drop ALL`, so the guest never holds a capability over this netns
/// (the kernel's `ns_capable()` walks from the netns's owner namespace, i.e.
/// pasta's, and stops there) and cannot touch the chain or reach nftables
/// (see [`crate::seccomp`], which keeps `AF_NETLINK` denied even with the
/// spike on).
///
/// The shim is fail closed: no `iptables`, a chain that cannot be created, or
/// a rule that does not verify all exit [`BOUNDARY_FAIL_EXIT`] **without**
/// starting the guest. It dumps the chain before and after the guest so the
/// caller (and `tests/spike_net_boundary.rs`) can prove the guest left it
/// untouched.
#[must_use]
pub fn boundary_script() -> String {
    let mut rules = String::new();
    for cidr in LAN_DROP_CIDRS {
        rules.push_str(&format!(
            "iptables -w -A {BOUNDARY_CHAIN} -d {cidr} -j DROP || fail add-{cidr}\n"
        ));
    }
    let mut checks = String::new();
    for cidr in LAN_DROP_CIDRS {
        checks.push_str(&format!(
            "iptables -w -C {BOUNDARY_CHAIN} -d {cidr} -j DROP || fail missing-{cidr}\n"
        ));
    }
    format!(
        r#"set -u
boundary_file=${{{env}:-}}
mark() {{ printf '{mark} %s\n' "$*" >&2; if [ -n "$boundary_file" ]; then printf '{mark} %s\n' "$*" >>"$boundary_file"; fi; }}
fail() {{ mark "boundary=failed reason=$1 exit={fail}"; exit {fail}; }}
dump() {{ {{ iptables -w -S {chain}; iptables -w -S OUTPUT; }} 2>&1 | tr '\n' ';'; }}
command -v iptables >/dev/null 2>&1 || fail no-iptables
mark "shim netns=$(readlink /proc/self/ns/net) userns=$(readlink /proc/self/ns/user) cap-eff=$(sed -n 's/^CapEff:[[:space:]]*//p' /proc/self/status)"
iptables -w -N {chain} 2>/dev/null || iptables -w -F {chain} || fail chain
{rules}iptables -w -C OUTPUT -j {chain} 2>/dev/null || iptables -w -A OUTPUT -j {chain} || fail output-jump
{checks}iptables -w -C OUTPUT -j {chain} || fail missing-output-jump
mark "rules=installed chain=$(dump)"
"$@"; rc=$?
mark "rules=after chain=$(dump) guest-exit=$rc"
exit $rc
"#,
        env = BOUNDARY_ENV,
        mark = BOUNDARY_MARK,
        fail = BOUNDARY_FAIL_EXIT,
        chain = BOUNDARY_CHAIN,
        rules = rules,
        checks = checks,
    )
}

// ---------------------------------------------------------------------------
// NET2: the relay arm (design B2) — `acos_netd::client`
// ---------------------------------------------------------------------------

/// The transport arm, from `ACOS_NET_ARM` (`relay` | `base`).
///
/// A typo is a structured error at the call site, never a silent switch of the
/// transport (the same rule `acos_netd::client::Arm::from_env` keeps).
pub fn arm() -> Result<acos_netd::client::Arm, String> {
    acos_netd::client::Arm::from_env().map_err(|e| e.message)
}

/// The environment variable that carries this session's id from the engine
/// (`acos-cli`'s ART engine) into the launcher process.
pub const SESSION_ID_ENV: &str = "ACOS_ART_SESSION_ID";

/// The relay session name for a session id — **one** rule for both paths.
///
/// NET4: the engine publishes a session id for *every* session it starts (the
/// `acos serve` id for an interactive session, its own run id for a one-shot
/// `verify`), the launcher names the relay session `acos-<id>` from
/// [`SESSION_ID_ENV`], and `acos_net` resolves the same `acos-<id>` from the id
/// its client holds. Before this the one-shot path fell back to `art-<pid>`,
/// which named a session no client could ever address — the network of a
/// `verify` run was unreachable even though it existed.
#[must_use]
pub fn net_session_name(session_id: &str) -> String {
    format!("acos-{session_id}")
}

/// The `acos-netd` binary: `ACOS_NETD_BIN`, else the one built next to this
/// executable (a workspace build puts both in the same target directory), else
/// `PATH`.
#[must_use]
pub fn netd_bin() -> std::ffi::OsString {
    if let Some(p) = std::env::var_os("ACOS_NETD_BIN") {
        return p;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("acos-netd");
            if cand.is_file() {
                return cand.into_os_string();
            }
        }
    }
    std::ffi::OsString::from("acos-netd")
}

/// The relay arm's command: `acos-netd run <name> -- <bwrap argv...>`.
///
/// Why a proxy process instead of asking the supervisor from here: the guest is
/// spawned by the relay supervisor (it owns the user namespace that holds the
/// session netns, see `acos_netd::supervisor`), so the launcher's `Child` is this
/// proxy. The proxy passes its own stdin/stdout/stderr over the control socket
/// (`SCM_RIGHTS`), so the guest's logcat and interaction pipes are the launcher's
/// own — capture is identical to spawning the guest directly — and it exits with
/// the guest's status. Killing it, or the launcher dying, tears the session down
/// (the supervisor detaches a session whose owner is gone).
#[must_use]
pub fn relay_wrap(bwrap_argv: &[std::ffi::OsString], session_id: &str) -> Command {
    let mut c = Command::new(netd_bin());
    c.arg("run");
    c.arg(net_session_name(session_id));
    c.arg("--");
    // `bwrap` is the program: `bwrap_argv` is its *argument list*, so the
    // program name has to be prepended here (exactly as `wrap` does).
    c.arg("bwrap");
    c.args(bwrap_argv);
    c
}

/// Wrap a bwrap argv in `pasta` + the boundary shim.
///
/// The returned command runs `/bin/sh -c '<boundary script>' sh bwrap ...`
/// under pasta. `/bin/sh` and `iptables` come from the host (pasta runs outside
/// the sandbox mount namespace), so they cost the guest nothing.
pub fn wrap(bwrap_argv: &[std::ffi::OsString]) -> Command {
    let program = std::env::var(PASTA_ENV).unwrap_or_else(|_| "pasta".to_string());
    let mut c = Command::new(program);
    c.args(pasta_args());
    c.arg("/bin/sh");
    c.arg("-c");
    c.arg(boundary_script());
    c.arg("sh");
    c.arg("bwrap");
    c.args(bwrap_argv);
    c
}

// ---------------------------------------------------------------------------
// dnsproxyd (netd replacement, minimal)
// ---------------------------------------------------------------------------

/// A running `dnsproxyd` responder for one directories-that-become-`/dev/socket`.
///
/// Dropping it stops the accept loop and removes the socket: the zygote recycles
/// a session's socket directory on every reset, and a responder left on the old
/// inode would both leak a thread per session and serve nobody.
pub struct DnsProxy {
    path: PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DnsProxy {
    /// The host path of the socket this responder serves.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DnsProxy {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        // Wake a blocking `accept`: a self-connect returns at once and the loop
        // then sees the stop flag. Nothing is served: the client sends nothing.
        let _ = UnixStream::connect(&self.path);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Serve `/dev/socket/dnsproxyd` on `sock_dir/dnsproxyd` until the returned
/// handle is dropped.
///
/// Android's bionic resolver does not read `/etc/resolv.conf`: `getaddrinfo`
/// serialises the query into a text request on this socket and expects netd to
/// answer (bionic `libc/dns/net/getaddrinfo.c`, `android_getaddrinfo_proxy`;
/// the wire format is netd's `DnsProxyListener`, `packages/modules/DnsResolver`).
/// The spike answers it with the host's `getaddrinfo`, i.e. the resolution
/// happens on the host side of the session, outside the guest.
///
/// **Which directory matters.** `sock_dir` must be a directory that a sandbox
/// binds at `/dev/socket`. There is more than one: the session launcher's, the
/// zygote's per-child `<plan>/<tag>/sock`, and `render-app`'s run socket dir.
/// Serving only the launcher's was the bug this signature exists to make
/// unfixable-by-accident: with the production zygote the app process mounts
/// `<plan>/<tag>/sock`, so the socket was invisible and `getaddrinfo` answered
/// `EAI_NODATA` without ever connecting (measured in `net-check`).
///
/// Request (NUL-terminated ASCII):
/// `getaddrinfo <host|^> <serv|^> <flags> <family> <socktype> <protocol> <netid>`
///
/// Answer: `222\0`, then per address `BE32 1, BE32 flags, family, socktype,
/// protocol, BE32 addrlen, addr bytes, BE32 namelen, name`, then `BE32 0`.
/// Any failure answers `500\0` + `000\0`, which bionic maps to `EAI_NODATA`.
pub fn serve_dnsproxyd(sock_dir: &Path) -> std::io::Result<DnsProxy> {
    std::fs::create_dir_all(sock_dir)?;
    let path = sock_dir.join("dnsproxyd");
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    // Device fidelity: `/dev/socket/dnsproxyd` is world-writable on Android
    // (`srw-rw-rw-`), because the resolver proxy is a service every app uid must
    // reach and SELinux, not DAC, is what restricts it there. A default 0755
    // bind here would deny `connect()` to the guest uid (10192) and bionic would
    // report `EAI_NODATA` without ever reaching this responder (measured).
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))?;
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("spike-dnsproxyd".into())
        .spawn(move || {
            for conn in listener.incoming() {
                if flag.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                match conn {
                    Ok(s) => {
                        // One request per connection is what bionic does
                        // (`fdopen` + `fclose`), so a short-lived thread per
                        // client is enough.
                        std::thread::spawn(move || {
                            if let Err(e) = handle(s) {
                                eprintln!("acos-art: spike dnsproxyd: {e}");
                            }
                        });
                    }
                    Err(e) => eprintln!("acos-art: spike dnsproxyd accept: {e}"),
                }
            }
        })?;
    Ok(DnsProxy { path, stop, thread: Some(thread) })
}

/// [`serve_dnsproxyd`] behind the spike flag: `None` when the flag is off, so
/// every call site stays byte-identical in behaviour without the gate.
pub fn serve_dnsproxyd_if_enabled(sock_dir: &Path) -> std::io::Result<Option<DnsProxy>> {
    if !enabled() {
        return Ok(None);
    }
    serve_dnsproxyd(sock_dir).map(Some)
}

fn handle(mut s: UnixStream) -> std::io::Result<()> {
    let mut req = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = s.read(&mut byte)?;
        if n == 0 || byte[0] == 0 {
            break;
        }
        req.push(byte[0]);
        if req.len() > 4096 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&req).to_string();
    let mut it = text.split(' ');
    let cmd = it.next().unwrap_or_default();
    if cmd != "getaddrinfo" {
        // `gethostbyname` is a separate netd command; bionic's
        // `gethostbyname` also goes through `android_getaddrinfo_proxy`, so the
        // spike answers it as an error instead of pretending.
        log_request(&text, "500 unhandled-command");
        s.write_all(b"500\0")?;
        s.write_all(b"000\0")?;
        s.shutdown(Shutdown::Both).ok();
        return Ok(());
    }
    let host = it.next().unwrap_or("^");
    let serv = it.next().unwrap_or("^");
    let num = |v: Option<&str>| v.and_then(|x| x.parse::<i32>().ok()).unwrap_or(-1);
    let flags = num(it.next());
    let family = num(it.next());
    let socktype = num(it.next());
    let protocol = num(it.next());

    // NET4: hand the guest only addresses the session can actually route.
    //
    // The session's interface is IPv4-only by construction — `plan::relay_pasta_args`
    // and `pasta_args` configure `10.163.x.x/30`, and `acos_services::connectivity`
    // (the constants `LinkProperties` reports to apps) has a single IPv4 address
    // and no IPv6 route. A resolver that also answers `AAAA` therefore describes a
    // network the session does not have: `HttpURLConnection` (default connect
    // timeout = infinite) picks the first address and hangs on an unroutable
    // IPv6 destination, so `net-check`'s status stayed `pending` for the whole
    // 60 s budget with the guest log showing no completed fetch (measured; one
    // `getaddrinfo example.com` answer carried 3 addresses).
    //
    // netd's resolver answers per-network capabilities the same way: a network
    // without IPv6 gets no AAAA records, not an address the app cannot reach.
    // When the relay grows IPv6 this filter goes with it (`session_has_ipv6`).
    let ipv6 = session_has_ipv6();
    let answers = resolve(host, serv, flags, family, socktype, protocol)
        .map(|answers| addresses_for_the_session(answers, ipv6));
    let Some(answers) = answers else {
        log_request(&text, "500 resolve-failed");
        s.write_all(b"500\0")?;
        s.write_all(b"000\0")?;
        s.shutdown(Shutdown::Both).ok();
        return Ok(());
    };
    log_request(&text, &format!("222 answers={}", answers.len()));
    let mut out = Vec::new();
    out.extend_from_slice(b"222\0");
    for a in answers {
        out.extend_from_slice(&1i32.to_be_bytes());
        out.extend_from_slice(&a.flags.to_be_bytes());
        out.extend_from_slice(&a.family.to_be_bytes());
        out.extend_from_slice(&a.socktype.to_be_bytes());
        out.extend_from_slice(&a.protocol.to_be_bytes());
        out.extend_from_slice(&(a.addr.len() as i32).to_be_bytes());
        out.extend_from_slice(&a.addr);
        out.extend_from_slice(&0i32.to_be_bytes()); // no canonname
    }
    out.extend_from_slice(&0i32.to_be_bytes());
    s.write_all(&out)?;
    s.shutdown(Shutdown::Both).ok();
    Ok(())
}

#[derive(Clone)]
struct Answer {
    flags: i32,
    family: i32,
    socktype: i32,
    protocol: i32,
    addr: Vec<u8>,
}

/// One line per answered request in the file named by [`DNS_LOG_ENV`] (no-op
/// when unset). Appends are `O_APPEND` writes of a short line, so concurrent
/// handler threads cannot interleave inside a line.
fn log_request(request: &str, outcome: &str) {
    use std::io::Write as _;
    let Some(path) = std::env::var_os(DNS_LOG_ENV) else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "req={request:?} -> {outcome}");
    }
}

/// Does the session's interface have a global IPv6 address?
///
/// It does not: [`pasta_args`]/`plan::relay_pasta_args` configure IPv4 only, and
/// [`acos_services::connectivity`] reports one IPv4 address. This is a function
/// (not a constant) so the day IPv6 is configured the DNS answer follows the
/// interface instead of staying behind.
#[must_use]
pub fn session_has_ipv6() -> bool {
    false
}

/// Drop the answers this session cannot route: `AAAA`/`AF_INET6` records while
/// the session has no IPv6 address. A query that asked for IPv6 only then has no
/// answers at all, which bionic reports as `EAI_NODATA` — the honest answer for
/// an IPv4-only network.
fn addresses_for_the_session(answers: Vec<Answer>, ipv6: bool) -> Vec<Answer> {
    if ipv6 {
        return answers;
    }
    answers.into_iter().filter(|answer| answer.family != libc::AF_INET6).collect()
}

/// Resolve one query with the host's resolver (`libc::getaddrinfo`).
fn resolve(
    host: &str,
    serv: &str,
    flags: i32,
    family: i32,
    socktype: i32,
    protocol: i32,
) -> Option<Vec<Answer>> {
    let c_host = if host == "^" { None } else { Some(std::ffi::CString::new(host).ok()?) };
    let c_serv = if serv == "^" { None } else { Some(std::ffi::CString::new(serv).ok()?) };
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_flags = if flags < 0 { 0 } else { flags };
    hints.ai_family = if family < 0 { libc::AF_UNSPEC } else { family };
    hints.ai_socktype = if socktype < 0 { 0 } else { socktype };
    hints.ai_protocol = if protocol < 0 { 0 } else { protocol };
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    let rc = unsafe {
        libc::getaddrinfo(
            c_host.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            c_serv.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            &hints,
            &mut res,
        )
    };
    if rc != 0 || res.is_null() {
        return None;
    }
    let mut out = Vec::new();
    let mut cur = res;
    while !cur.is_null() {
        let ai = unsafe { &*cur };
        if !ai.ai_addr.is_null() && ai.ai_addrlen > 0 {
            let bytes = unsafe {
                std::slice::from_raw_parts(ai.ai_addr as *const u8, ai.ai_addrlen as usize)
            };
            out.push(Answer {
                flags: ai.ai_flags,
                family: ai.ai_family,
                socktype: ai.ai_socktype,
                protocol: ai.ai_protocol,
                addr: bytes.to_vec(),
            });
        }
        cur = ai.ai_next;
    }
    unsafe { libc::freeaddrinfo(res) };
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// The path the spike's DNS socket is placed at (`ACOS_SPIKE_NET_DNS` overrides
/// it; the session uses its own `/dev/socket` directory).
#[must_use]
pub fn dns_socket_env_path() -> Option<PathBuf> {
    std::env::var_os("ACOS_SPIKE_NET_DNS").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shim installs every LAN range, verifies it rule by rule, and only
    /// then starts the guest: an unguarded session is not possible.
    #[test]
    fn boundary_script_installs_verifies_and_fails_closed() {
        let s = boundary_script();
        for cidr in LAN_DROP_CIDRS {
            let rule = format!("-d {cidr} -j DROP");
            assert_eq!(
                s.matches(&rule).count(),
                2,
                "{cidr} must be added once and checked once"
            );
            assert!(
                s.contains(&format!(
                    "iptables -w -A {BOUNDARY_CHAIN} {rule} || fail add-{cidr}"
                )),
                "the add of {cidr} must be fail closed"
            );
            assert!(
                s.contains(&format!(
                    "iptables -w -C {BOUNDARY_CHAIN} {rule} || fail missing-{cidr}"
                )),
                "the check of {cidr} must be fail closed"
            );
        }
        // No iptables at all: the session must not start.
        assert!(s.contains("command -v iptables >/dev/null 2>&1 || fail no-iptables"));
        // The guest (`"$@"`) runs only after the last verification, and every
        // failure path exits before it.
        let run = s.find("\"$@\"").expect("the guest invocation");
        assert!(s.rfind("|| fail").expect("a failure path") < run);
        assert!(s.contains(&format!("exit {BOUNDARY_FAIL_EXIT}")));
        // The chain is dumped before and after the guest, so a change is
        // visible: `tests/spike_net_boundary.rs` compares the two dumps.
        let installed = s.find("rules=installed").expect("the before dump");
        let after = s.find("rules=after").expect("the after dump");
        assert!(installed < run && run < after);
        // The OUTPUT jump is what makes the chain effective for guest traffic.
        assert!(s.contains(&format!("iptables -w -A OUTPUT -j {BOUNDARY_CHAIN}")));
        assert!(s.contains(&format!(
            "iptables -w -C OUTPUT -j {BOUNDARY_CHAIN} || fail"
        )));
    }

    /// The marker file is opt-in (a host path), and the guest never sees it:
    /// bwrap runs with `--clearenv`.
    #[test]
    fn boundary_marker_file_is_opt_in() {
        let s = boundary_script();
        assert!(s.contains(&format!("boundary_file=${{{BOUNDARY_ENV}:-}}")));
        assert!(s.contains("if [ -n \"$boundary_file\" ]"));
    }

    /// pasta's hardening flags are all present (ADR-0030 amendment).
    #[test]
    fn pasta_args_keep_host_loopback_closed() {
        let args = pasta_args();
        assert!(
            args.iter().any(|a| a == "--no-splice"),
            "the loopback splice must be off"
        );
        for (flag, value) in [
            ("-T", "none"),
            ("-U", "none"),
            ("--map-host-loopback", "none"),
        ] {
            let at = args
                .iter()
                .position(|a| a == flag)
                .unwrap_or_else(|| panic!("{flag}"));
            assert_eq!(args[at + 1], value, "{flag} must be {value}");
        }
        assert_eq!(
            args.last().map(String::as_str),
            Some("--"),
            "the pasta argv ends at --"
        );
    }
}

#[cfg(test)]
mod dns_tests {
    use super::*;
    use std::io::Write as _;

    /// A short, private directory: an AF_UNIX `sun_path` holds 108 bytes, and the
    /// checkout path is longer than that. This is the product's own convention
    /// (`ShortSocketDir` uses `/run/user/<uid>` for the same reason), not `/tmp`.
    fn scratch(tag: &str) -> PathBuf {
        let uid = unsafe { libc::getuid() };
        let dir = PathBuf::from(format!("/run/user/{uid}/acos-dns-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a short private socket dir");
        dir
    }

    /// The responder must be reachable by an app uid: `/dev/socket/dnsproxyd` is
    /// world-writable on a device (`srw-rw-rw-`) because every app connects to
    /// it. A 0755 bind denies `connect()` to the guest uid and bionic then
    /// answers `EAI_NODATA` without ever reaching this code — measured as the
    /// first wall of session 3.
    #[test]
    fn dnsproxyd_socket_is_world_writable_and_answers() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("dns");
        let proxy = serve_dnsproxyd(&dir).expect("the responder binds");
        let mode = std::fs::symlink_metadata(proxy.path())
            .expect("the socket exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o666, "dnsproxyd must be world-writable, got {mode:o}");

        // Exactly the request bionic's `android_getaddrinfo_proxy` writes
        // (`getaddrinfo <host> <serv> <flags> <family> <socktype> <protocol>
        // <netid>` + NUL).
        let mut s = UnixStream::connect(proxy.path()).expect("connect");
        s.write_all(b"getaddrinfo localhost ^ 1024 0 1 0 0\0").expect("request");
        s.shutdown(Shutdown::Write).ok();
        let mut head = [0u8; 4];
        s.read_exact(&mut head).expect("a query result code");
        assert_eq!(&head[..3], b"222", "DnsProxyQueryResult, got {head:?}");
        let mut more = [0u8; 4];
        s.read_exact(&mut more).expect("the first address marker");
        assert_eq!(i32::from_be_bytes(more), 1, "at least one address");
        drop(s);

        // Dropping the handle stops the loop and removes the socket: the zygote
        // recycles a session's socket dir on every reset.
        let path = proxy.path().to_path_buf();
        drop(proxy);
        assert!(!path.exists(), "the socket is removed when the session ends");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// NET4: the resolver must not hand the guest an address family its interface
    /// does not have.
    ///
    /// The session is IPv4-only (`pasta_args`/`plan` configure `10.163.x.x/30`,
    /// `LinkProperties` reports one IPv4 address), but the host resolver answers
    /// `getaddrinfo example.com` with A *and* AAAA records. `HttpURLConnection`
    /// has no connect timeout by default, so an unroutable first IPv6 address
    /// left `net-check`'s status `pending` until the scenario's 60 s budget
    /// expired (`logs/logcat.txt` had no completed fetch line at all).
    #[test]
    fn the_resolver_never_answers_an_address_family_the_session_lacks() {
        assert!(!session_has_ipv6(), "the session interface is IPv4-only");
        let answers = resolve("example.com", "^", 0, libc::AF_UNSPEC, 0, 0)
            .expect("the host resolves example.com (this test needs host DNS)");
        assert!(!answers.is_empty());
        let kept = addresses_for_the_session(answers.clone(), false);
        assert!(
            kept.iter().all(|answer| answer.family == libc::AF_INET),
            "an IPv4-only session must get no AAAA answer"
        );
        assert!(!kept.is_empty(), "the A records are still returned");
        assert_eq!(
            addresses_for_the_session(answers.clone(), true).len(),
            answers.len(),
            "with IPv6 configured nothing is filtered"
        );
        // An IPv6-only query has no answer on this session (bionic: EAI_NODATA),
        // instead of an address the guest cannot route.
        if let Some(v6) = resolve("example.com", "^", 0, libc::AF_INET6, 0, 0) {
            assert!(addresses_for_the_session(v6, false).is_empty());
        }
    }

    /// The addresses pasta configures and the addresses `LinkProperties` reports
    /// are one set of constants, so the interface an app reads cannot drift from
    /// the interface the namespace has.
    #[test]
    fn pasta_args_use_the_connectivity_constants() {
        use acos_services::connectivity::{
            NET_DNS_SERVER, NET_GUEST_ADDRESS, NET_GUEST_GATEWAY, NET_GUEST_PREFIX_LEN,
        };
        let ip = |a: [u8; 4]| format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]);
        let args = pasta_args();
        for (flag, expected) in [
            ("-a", format!("{}/{}", ip(NET_GUEST_ADDRESS), NET_GUEST_PREFIX_LEN)),
            ("-g", ip(NET_GUEST_GATEWAY)),
            ("-D", ip(NET_DNS_SERVER)),
        ] {
            let at = args.iter().position(|a| a == flag).unwrap_or_else(|| panic!("{flag}"));
            assert_eq!(args[at + 1], expected, "{flag} must match LinkProperties");
        }
    }
}
