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
use std::net::{Ipv4Addr, Shutdown};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment switch for the whole spike (`1` = on, unset = off).
pub const FLAG: &str = "ACOS_SPIKE_NET";
/// Path of the `pasta` binary the spike uses (default: `pasta` in `PATH`).
pub const PASTA_ENV: &str = "ACOS_SPIKE_NET_PASTA";

/// True when the spike is switched on. The only gate in the module.
#[must_use]
pub fn enabled() -> bool {
    matches!(std::env::var(FLAG).as_deref(), Ok("1") | Ok("true"))
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
/// * `-a 169.254.2.15/24 -g 169.254.2.1` — the guest gets a link-local
///   address, not the host's LAN address, so nothing in the guest looks like it
///   is on the LAN.
/// * `-D 1.1.1.1` — the DHCP/NDP DNS answer handed to the guest.
#[must_use]
pub fn pasta_args() -> Vec<String> {
    [
        "-f",
        "-q",
        "--config-net",
        "-a",
        "169.254.2.15/24",
        "-g",
        "169.254.2.1",
        "--no-map-gw",
        "--map-host-loopback",
        "none",
        "-T",
        "none",
        "-U",
        "none",
        "--no-splice",
        "-D",
        "1.1.1.1",
        "--",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// RFC1918 + link-local destinations are dropped in the session's own netns.
///
/// `pasta` forwards *any* destination the guest sends it, the LAN included
/// (measured: `192.168.31.1:80 -> OK` with the hardened flags above). There is
/// no destination ACL in passt, so the spike enforces it where the kernel can:
/// the netns is owned by pasta's user namespace, so the command it spawns has
/// `CAP_NET_ADMIN` over that netns only, which is enough for `iptables`.
pub const LAN_GUARD: &str = "iptables -w -A OUTPUT -d 192.168.0.0/16 -j DROP; \
iptables -w -A OUTPUT -d 10.0.0.0/8 -j DROP; \
iptables -w -A OUTPUT -d 172.16.0.0/12 -j DROP; \
iptables -w -A OUTPUT -d 169.254.0.0/16 -j DROP";

/// Wrap a bwrap argv in `pasta` + the LAN guard.
///
/// The returned command runs `/bin/sh -c '<guard>; exec "$@"' sh bwrap ...`
/// under pasta. `/bin/sh` comes from the host (pasta runs outside the sandbox
/// mount namespace), so it costs the guest nothing.
pub fn wrap(bwrap_argv: &[std::ffi::OsString]) -> Command {
    let program = std::env::var(PASTA_ENV).unwrap_or_else(|_| "pasta".to_string());
    let mut c = Command::new(program);
    c.args(pasta_args());
    c.arg("/bin/sh");
    c.arg("-c");
    c.arg(format!("{LAN_GUARD}; exec \"$@\""));
    c.arg("sh");
    c.arg("bwrap");
    c.args(bwrap_argv);
    c
}

// ---------------------------------------------------------------------------
// dnsproxyd (netd replacement, minimal)
// ---------------------------------------------------------------------------

/// Serve `/dev/socket/dnsproxyd` on `sock_dir/dnsproxyd` until the process
/// exits.
///
/// Android's bionic resolver does not read `/etc/resolv.conf`: `getaddrinfo`
/// serialises the query into a text request on this socket and expects netd to
/// answer (bionic `libc/dns/net/getaddrinfo.c`, `android_getaddrinfo_proxy`;
/// the wire format is netd's `DnsProxyListener`, `packages/modules/DnsResolver`).
/// The spike answers it with the host's `getaddrinfo`, i.e. the resolution
/// happens on the host side of the session, outside the guest.
///
/// Request (NUL-terminated ASCII):
/// `getaddrinfo <host|^> <serv|^> <flags> <family> <socktype> <protocol> <netid>`
///
/// Answer: `222\0`, then per address `BE32 1, BE32 flags, family, socktype,
/// protocol, BE32 addrlen, addr bytes, BE32 namelen, name`, then `BE32 0`.
/// Any failure answers `500\0` + `000\0`, which bionic maps to `EAI_NODATA`.
pub fn serve_dnsproxyd(sock_dir: &Path) -> std::io::Result<std::thread::JoinHandle<()>> {
    let path = sock_dir.join("dnsproxyd");
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::thread::Builder::new().name("spike-dnsproxyd".into()).spawn(move || {
        for conn in listener.incoming() {
            match conn {
                Ok(s) => {
                    // One request per connection is what bionic does (`fdopen`
                    // + `fclose`), so a short-lived thread per client is enough.
                    std::thread::spawn(move || {
                        if let Err(e) = handle(s) {
                            eprintln!("acos-art: spike dnsproxyd: {e}");
                        }
                    });
                }
                Err(e) => eprintln!("acos-art: spike dnsproxyd accept: {e}"),
            }
        }
    })
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

    let answers = resolve(host, serv, flags, family, socktype, protocol);
    let Some(answers) = answers else {
        s.write_all(b"500\0")?;
        s.write_all(b"000\0")?;
        s.shutdown(Shutdown::Both).ok();
        return Ok(());
    };
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

struct Answer {
    flags: i32,
    family: i32,
    socktype: i32,
    protocol: i32,
    addr: Vec<u8>,
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

/// The guest's loopback for the LAN guard's own address (unused placeholder to
/// keep the address in one place for the report/tests).
pub const GUEST_ADDR: Ipv4Addr = Ipv4Addr::new(169, 254, 2, 15);
