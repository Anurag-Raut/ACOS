//! SPIKE-NET boundary: the LAN drop rules are installed **before any guest
//! code runs**, and the guest cannot change them.
//!
//! ADR-0030's amendment (2026-11-16) requires the RFC1918/link-local/CGNAT drop
//! to be a real boundary, not a policy the guest can undo. Three claims, each
//! failing if its layer is removed:
//!
//! 1. **The rules exist before the guest starts.** The setup shim runs inside
//!    the session's own netns — it is the command `pasta` spawns, so it is root
//!    *in pasta's user namespace* and holds `CAP_NET_ADMIN` over that netns. It
//!    installs the chain, verifies it rule by rule, and only then exec's bwrap.
//!    Any failure exits `111` and the guest is never started.
//! 2. **The guest holds no capability over that netns.** bwrap puts the guest
//!    in a *child* user namespace with `--cap-drop ALL`; this test reads
//!    `/proc/<guest>/status` (`CapEff: 0`) and compares the guest's user
//!    namespace inode with the shim's (the netns owner's), and the guest itself
//!    proves the kernel refuses `SO_MARK` (checked against
//!    `sock_net(sk)->user_ns`) and `SOCK_RAW`.
//! 3. **The guest cannot reach netfilter at all.** `AF_NETLINK` stays denied by
//!    the seccomp filter even with the spike on (the nft backend of `iptables`
//!    speaks netlink), and there is no `iptables` binary in the mount view to
//!    exec. The shim dumps the chain again after the guest exits and the two
//!    dumps must be identical.
//!
//! The probe is a host-built dynamic binary bound read-only into the real guest
//! mount view (same construction as `tests/sandbox_s6.rs`), so what it observes
//! is what untrusted guest code observes. Requires `pasta` (`.cache/spike-net/
//! bin/pasta`, or `ACOS_SPIKE_NET_PASTA`); the test skips with a message when
//! it is missing, like the ART image tests skip without a sysroot.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use acos_art::ArtRoot;

static ONE_GUEST: Mutex<()> = Mutex::new(());

fn guard() -> MutexGuard<'static, ()> {
    ONE_GUEST.lock().unwrap_or_else(|e| e.into_inner())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the crate lives at <repo>/crates/acos-art")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join(".cache/spike-net-tests")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch under .cache/");
    dir
}

/// `pasta` for this run: `ACOS_SPIKE_NET_PASTA` when named (verbatim), else the
/// product's own lookup (`acos_netd_ctl::pasta`: `ACOS_NET_PASTA`,
/// `<root>/libexec/pasta`, `$ACOS_PREFIX`'s `current`, `PATH`), else the spike's
/// build cache.
fn pasta_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ACOS_SPIKE_NET_PASTA") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    if let Some(p) = acos_netd::pasta::resolved_path() {
        return Some(p);
    }
    let p = repo_root().join(".cache/spike-net/bin/pasta");
    p.is_file().then_some(p)
}

/// The guest probe: `argv[1]` selects the mode.
///
/// Dynamic on purpose: host glibc is bound into the guest by `guest_command`.
/// Every line is `probe: <key> <verdict>`, so the assertions are literal.
const PROBE_SRC: &str = r#"#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <sys/time.h>
#include <sys/wait.h>

#ifndef SO_MARK
#define SO_MARK 36
#endif

static void status_field(const char *key, char *out, size_t n)
{
    char buf[16384];
    int fd = open("/proc/self/status", O_RDONLY);
    ssize_t r;
    out[0] = 0;
    if (fd < 0) return;
    r = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (r <= 0) return;
    buf[r] = 0;
    {
        char *line = strtok(buf, "\n");
        while (line) {
            if (strncmp(line, key, strlen(key)) == 0) {
                const char *v = line + strlen(key);
                while (*v == ' ' || *v == '\t') v++;
                snprintf(out, n, "%s", v);
                return;
            }
            line = strtok(NULL, "\n");
        }
    }
}

static void ns_inode(const char *path, char *out, size_t n)
{
    char buf[128];
    ssize_t r = readlink(path, buf, sizeof buf - 1);
    out[0] = 0;
    if (r <= 0) return;
    buf[r] = 0;
    /* "user:[4026532543]" -> "4026532543" */
    {
        char *b = strchr(buf, '[');
        char *e = strrchr(buf, ']');
        if (b && e && e > b) { *e = 0; snprintf(out, n, "%s", b + 1); }
        else snprintf(out, n, "%s", buf);
    }
}

static int connect_with_timeout(int family, const char *ip, int port, int secs)
{
    int fd, rc;
    struct sockaddr_in sa;
    struct timeval tv;
    fd = socket(family, SOCK_STREAM, 0);
    if (fd < 0) return -errno;
    tv.tv_sec = secs; tv.tv_usec = 0;
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv);
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_port = htons((unsigned short)port);
    if (inet_pton(AF_INET, ip, &sa.sin_addr) != 1) { close(fd); return -EINVAL; }
    rc = connect(fd, (struct sockaddr *)&sa, sizeof sa);
    if (rc == 0) { close(fd); return 0; }
    rc = -errno;
    close(fd);
    return rc;
}

int main(int argc, char **argv)
{
    const char *mode = argc > 1 ? argv[1] : "";
    char cap[64], userns[64], netns[64];
    int fd, err;

    setvbuf(stdout, NULL, _IOLBF, 0);
    if (strcmp(mode, "net-guard") != 0) return 2;

    status_field("CapEff:", cap, sizeof cap);
    ns_inode("/proc/self/ns/user", userns, sizeof userns);
    ns_inode("/proc/self/ns/net", netns, sizeof netns);
    printf("probe: uid %d\n", (int)getuid());
    printf("probe: cap-eff %s\n", cap);
    printf("probe: userns %s\n", userns);
    printf("probe: netns %s\n", netns);

    /* 1. the transport itself must work with the guard installed */
    err = connect_with_timeout(AF_INET, "1.1.1.1", 443, 4);
    printf("probe: connect-public %s\n", err == 0 ? "OK" : strerror(-err));
    /* 2. the LAN must be dropped by the netns's own netfilter */
    err = connect_with_timeout(AF_INET, "192.168.31.1", 80, 2);
    printf("probe: connect-lan %s\n", err == 0 ? "REACHABLE" : strerror(-err));
    /* 3. host loopback must stay unreachable (`-T none --no-splice`) */
    err = connect_with_timeout(AF_INET, "127.0.0.1", 18080, 2);
    printf("probe: connect-host-loopback %s\n", err == 0 ? "REACHABLE" : strerror(-err));

    /* 4. netfilter: the nft backend speaks AF_NETLINK; seccomp denies it */
    errno = 0;
    fd = socket(AF_NETLINK, SOCK_RAW, 12 /* NETLINK_NETFILTER */);
    printf("probe: socket-netlink %s\n",
           fd >= 0 ? "ALLOWED" : (errno == EACCES ? "DENIED:EACCES" : strerror(errno)));
    if (fd >= 0) close(fd);

    /* 5. no CAP_NET_ADMIN over the session netns: SO_MARK is checked against
     *    sock_net(sk)->user_ns, which is the netns owner (pasta's userns) */
    fd = socket(AF_INET, SOCK_STREAM, 0);
    printf("probe: socket-inet %s\n", fd >= 0 ? "ALLOWED" : "DENIED");
    if (fd >= 0) {
        int mark = 1;
        errno = 0;
        err = setsockopt(fd, SOL_SOCKET, SO_MARK, &mark, sizeof mark);
        printf("probe: so-mark %s\n",
               err == 0 ? "ALLOWED" : (errno == EPERM ? "DENIED:EPERM" : strerror(errno)));
        close(fd);
    }
    /* 6. SOCK_RAW needs CAP_NET_RAW in the same namespace */
    errno = 0;
    fd = socket(AF_INET, SOCK_RAW, IPPROTO_TCP);
    printf("probe: socket-raw-inet %s\n",
           fd >= 0 ? "ALLOWED" : (errno == EPERM ? "DENIED:EPERM" : strerror(errno)));
    if (fd >= 0) close(fd);

    /* 7. try to flush the chain from inside the guest. Production binds no
     *    /usr, so there is nothing to exec there; this test binds /usr for
     *    host glibc, which makes the attempt possible — and it must fail:
     *    AF_NETLINK is denied by seccomp and the process holds no
     *    CAP_NET_ADMIN over the netns (SO_MARK above). */
    fd = open("/usr/sbin/iptables", O_RDONLY);
    printf("probe: iptables-binary %s\n", fd >= 0 ? "PRESENT" : "MISSING");
    if (fd >= 0) {
        int fds[2];
        pid_t pid;
        int st = 0;
        close(fd);
        if (pipe(fds) == 0) {
            pid = fork();
            if (pid == 0) {
                close(fds[0]);
                dup2(fds[1], 1);
                dup2(fds[1], 2);
                close(fds[1]);
                execl("/usr/sbin/iptables", "iptables", "-w", "-F", "acos-net-drop",
                      (char *)0);
                printf("exec failed: %s\n", strerror(errno));
                _exit(127);
            }
            close(fds[1]);
            {
                char out[512];
                ssize_t n = read(fds[0], out, sizeof out - 1);
                close(fds[0]);
                waitpid(pid, &st, 0);
                printf("probe: iptables-flush exit=%d\n",
                       (WIFEXITED(st)) ? WEXITSTATUS(st) : -1);
                if (n > 0) {
                    out[n] = 0;
                    printf("probe: iptables-flush-output %s\n", out);
                }
            }
        }
    }

    printf("probe: done\n");
    fflush(stdout);
    /* Stay alive long enough for the host side to read /proc/<guest>. */
    sleep(6);
    return 0;
}
"#;

fn build_probe(dir: &Path) -> PathBuf {
    let src = dir.join("probe.c");
    let out = dir.join("probe");
    if out.is_file() {
        return out;
    }
    std::fs::write(&src, PROBE_SRC).expect("write the probe");
    let st = Command::new("cc")
        .args(["-O1", "-o"])
        .arg(&out)
        .arg(&src)
        .status()
        .expect("host cc");
    assert!(st.success(), "the guest probe must compile");
    out
}

/// An `ArtRoot` with the guard artefacts (`<data>/seccomp/...`) built.
fn probe_root(tag: &str) -> Option<ArtRoot> {
    let dir = scratch(&format!("{tag}-root"));
    let root = match ArtRoot::resolve(None, Some(dir.join("data"))) {
        Ok(r) => r,
        Err(e) => {
            acos_test_support::require_prereq(
                false,
                &format!("{tag} (spike net boundary)"),
                &format!("no ART sysroot ({e}) — run tools/art-sysroot.sh"),
            )?;
            return None;
        }
    };
    root.ensure_data().expect("data skeleton");
    acos_art::seccomp::build_guest(&root.data.join("seccomp")).expect("guard artefacts");
    Some(root)
}

/// The real guest argv, wrapped by `spike_net::wrap` (pasta + boundary shim).
fn guest_command(root: &ArtRoot, run: &Path, probe: &Path, mode: &str) -> Command {
    let mut sb = root.sandbox();
    sb.run_root(run);
    sb.seccomp_guest(true);
    sb.with_shim(false);
    sb.ro_bind(probe, "/data/probe");
    for host in ["/usr", "/lib", "/lib64"] {
        if Path::new(host).is_dir() {
            sb.ro_bind(host, host);
        }
    }
    sb.args(Vec::new());
    sb.command("/data/probe", &[mode.to_string()])
        .expect("sandbox argv")
}

// ---------------------------------------------------------------------------
// host-side /proc inspection
// ---------------------------------------------------------------------------

fn read_ppid(pid: u32) -> Option<u32> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("PPid:"))
        .and_then(|v| v.trim().parse().ok())
}

fn all_pids() -> Vec<u32> {
    std::fs::read_dir("/proc")
        .map(|d| {
            d.filter_map(|e| {
                let name = e.ok()?.file_name().to_string_lossy().to_string();
                name.parse::<u32>().ok()
            })
            .collect()
        })
        .unwrap_or_default()
}

/// Every live descendant of `root`, transitively.
fn descendants(root: u32) -> Vec<u32> {
    let pids = all_pids();
    let mut out = vec![root];
    let mut grew = true;
    while grew {
        grew = false;
        for pid in &pids {
            if out.contains(pid) {
                continue;
            }
            if let Some(ppid) = read_ppid(*pid) {
                if out.contains(&ppid) {
                    out.push(*pid);
                    grew = true;
                }
            }
        }
    }
    out.retain(|p| *p != root);
    out
}

fn read_status_field(pid: u32, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(key))
        .map(|v| v.trim().to_string())
}

fn ns_inode_of(pid: u32, ns: &str) -> Option<String> {
    let target = std::fs::read_link(format!("/proc/{pid}/ns/{ns}")).ok()?;
    let s = target.to_string_lossy().to_string();
    let b = s.find('[')?;
    let e = s.rfind(']')?;
    Some(s[b + 1..e].to_string())
}

fn cmdline(pid: u32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default()
}

/// What the host observed about the running guest processes.
struct GuestProcs {
    seen: Vec<(u32, String, Option<String>, Option<String>)>, // pid, cmdline, cap-eff frac, userns
}

fn sample_guest_processes(child_pid: u32, timeout: Duration) -> GuestProcs {
    let deadline = Instant::now() + timeout;
    let mut seen: Vec<(u32, String, Option<String>, Option<String>)> = Vec::new();
    while Instant::now() < deadline {
        for pid in descendants(child_pid) {
            let cl = cmdline(pid);
            // The shim's own cmdline is `/bin/sh -c <script> sh bwrap ... /data/probe`
            // (it *contains* the probe path); the guest's is `/data/probe net-guard`.
            if !cl.trim_start().starts_with("/data/probe") {
                continue;
            }
            if seen.iter().any(|(p, ..)| *p == pid) {
                continue;
            }
            seen.push((
                pid,
                cl,
                read_status_field(pid, "CapEff:"),
                ns_inode_of(pid, "user"),
            ));
        }
        if seen.is_empty() {
            std::thread::sleep(Duration::from_millis(25));
        } else {
            break;
        }
    }
    GuestProcs { seen }
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(st) = child.try_wait().expect("wait") {
            return Some(st);
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// the boundary
// ---------------------------------------------------------------------------

struct Run {
    status: Option<std::process::ExitStatus>,
    stdout: String,
    stderr: String,
    boundary: String,
    /// Host-side `/proc` view of the guest processes (`pid`, cmdline, CapEff,
    /// userns inode), captured while the guest was alive.
    procs: GuestProcs,
}

fn run_guest(tag: &str, mode: &str, timeout: Duration) -> Option<Run> {
    let pasta = pasta_binary()?;
    std::env::set_var("ACOS_SPIKE_NET", "1");
    // These tests prove the per-session `pasta` arm's boundary shim. The default
    // arm became the shared relay (NET2/NET4), which needs a session id and its
    // own supervisor; without naming `base` the tests failed at `sandbox argv`
    // the first time a pinned pasta was present (NETREAL).
    std::env::set_var("ACOS_NET_ARM", "base");
    std::env::set_var("ACOS_SPIKE_NET_PASTA", &pasta);

    let root = probe_root(tag)?;
    let dir = scratch(&format!("{tag}-run"));
    let probe = build_probe(&dir);
    let run = root.new_run_dir(tag).expect("run dir");
    let boundary_path = dir.join("net-boundary.txt");
    std::env::set_var("ACOS_SPIKE_NET_BOUNDARY", &boundary_path);
    let out_path = dir.join("probe.stdout");
    let err_path = dir.join("probe.stderr");
    let out = std::fs::File::create(&out_path).expect("stdout file");
    let err = std::fs::File::create(&err_path).expect("stderr file");
    let mut guest = guest_command(&root, run.path(), &probe, mode);
    guest.current_dir(&dir);
    guest
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));

    // A host loopback listener the guest must not reach: pasta's defaults
    // (mirror + splice of every loopback port) would expose it.
    let sentinel = std::net::TcpListener::bind("127.0.0.1:18080").ok();

    let mut child = guest.spawn().expect("spawn pasta");
    let pid = child.id();
    let procs = sample_guest_processes(pid, Duration::from_secs(20));
    let status = wait_with_timeout(&mut child, timeout);
    if let Some(listener) = sentinel {
        drop(listener);
    }
    let stdout = std::fs::read_to_string(&out_path).unwrap_or_default();
    let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
    let boundary = std::fs::read_to_string(&boundary_path).unwrap_or_default();
    Some(Run {
        status,
        stdout,
        stderr,
        boundary,
        procs,
    })
}

fn assert_line(text: &str, needle: &str) {
    assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
}

/// The dump the shim writes before the guest starts, as a set of rules.
fn rule_set(dump: &str) -> Vec<String> {
    let mut rules: Vec<String> = dump
        .split(';')
        .map(str::trim)
        .filter(|r| r.starts_with("-A "))
        .map(str::to_string)
        .collect();
    rules.sort();
    rules.dedup();
    rules
}

#[test]
fn guest_cannot_change_the_lan_drop_rules() {
    let _g = guard();
    let skipped_before = acos_test_support::skipped_count();
    acos_test_support::require_prereq(
        pasta_binary().is_some(),
        "spike net boundary (LAN rules)",
        "no pasta binary (set ACOS_SPIKE_NET_PASTA or build .cache/spike-net/bin/pasta)",
    );
    if acos_test_support::skipped_count() != skipped_before {
        return;
    }
    let Some(run) = run_guest("boundary", "net-guard", Duration::from_secs(90)) else {
        acos_test_support::require_prereq(
            false,
            "spike net boundary (LAN rules)",
            "no ART sysroot — run tools/art-sysroot.sh",
        );
        return;
    };
    eprintln!("--- probe stdout ---\n{}", run.stdout);
    eprintln!("--- shim stderr ---\n{}", run.stderr);
    eprintln!("--- boundary ---\n{}", run.boundary);

    assert_eq!(
        run.status.and_then(|s| s.code()),
        Some(0),
        "the probe must exit 0 (shim stderr:\n{})",
        run.stderr
    );
    assert_line(&run.stdout, "probe: done");

    // 1. the rules existed before the guest started, all five ranges plus the
    //    OUTPUT jump, and they are still there after it exited.
    assert_line(&run.boundary, "rules=installed");
    let before_start = run
        .boundary
        .split("rules=installed ")
        .nth(1)
        .expect("the shim dump after installing");
    let before_dump = before_start.split('\n').next().unwrap_or_default();
    let before = rule_set(&before_dump);
    for cidr in acos_art::spike_net::LAN_DROP_CIDRS {
        assert!(
            before
                .iter()
                .any(|r| r.contains(cidr) && r.ends_with("-j DROP")),
            "missing DROP {cidr} in the installed chain: {before:?}"
        );
    }
    assert!(
        before.iter().any(|r| r == "-A OUTPUT -j acos-net-drop"),
        "the OUTPUT chain must jump into the drop chain: {before:?}"
    );
    let after = rule_set(
        run.boundary
            .split("rules=after ")
            .nth(1)
            .expect("the shim dump after the guest")
            .split('\n')
            .next()
            .unwrap_or_default(),
    );
    assert_eq!(after, before, "the guest changed the boundary chain");

    // 2. the shim (the netns's owner namespace) *did* have the capability, so
    //    the rules are not a coincidence of a universe without iptables.
    let shim_line = run
        .boundary
        .lines()
        .find(|l| l.contains("shim netns="))
        .expect("the shim's own /proc/self/ns + CapEff line");
    assert!(
        !shim_line.contains("cap-eff=0000000000000000"),
        "the shim must hold CAP_NET_ADMIN over the netns: {shim_line}"
    );
    let owner_userns = shim_line
        .split("userns=")
        .nth(1)
        .and_then(|v| v.split_whitespace().next())
        .expect("the shim's userns inode");

    // 3. the guest: no caps, a *child* user namespace, and the kernel refuses
    //    every privileged net operation the guest could use to undo the rules.
    let cap = run
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("probe: cap-eff "))
        .expect("CapEff from the guest");
    assert_eq!(
        cap.trim(),
        "0000000000000000",
        "the guest must hold no capabilities"
    );
    let guest_userns = run
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("probe: userns "))
        .expect("the guest's userns inode");
    assert_ne!(
        guest_userns.trim(),
        owner_userns,
        "the guest must not run in the netns owner's user namespace"
    );
    assert_line(&run.stdout, "probe: socket-netlink DENIED:EACCES");
    assert_line(&run.stdout, "probe: so-mark DENIED:EPERM");
    assert_line(&run.stdout, "probe: socket-raw-inet DENIED:EPERM");
    // The guest tried the real thing: `iptables -F` on the chain it cannot
    // see. The exec is present only because this test binds /usr for glibc;
    // production binds none of it.
    assert_line(&run.stdout, "probe: iptables-binary PRESENT");
    let flush = run
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("probe: iptables-flush exit="))
        .expect("the guest's iptables -F attempt");
    assert_ne!(
        flush.trim(),
        "0",
        "the guest flushed the boundary chain: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("Operation not permitted")
            || run.stdout.contains("Permission denied")
            || run.stdout.contains("Failed to initialize"),
        "the guest's flush must fail with a permission error, got:\n{}",
        run.stdout
    );

    // 3b. the same facts read from /proc by the *host* while the guest ran: no
    //     guest process holds a capability, and none is in the netns owner's
    //     user namespace.
    assert!(
        !run.procs.seen.is_empty(),
        "the host never observed the guest process in /proc"
    );
    for (pid, cmd, cap, userns) in &run.procs.seen {
        eprintln!("guest pid {pid} cmd={cmd:?} cap-eff={cap:?} userns={userns:?}");
        assert_eq!(
            cap.as_deref().map(str::trim),
            Some("0000000000000000"),
            "pid {pid} must hold no capabilities"
        );
        assert_eq!(
            userns.as_deref(),
            Some(guest_userns.trim()),
            "pid {pid} must be in the guest's own user namespace"
        );
    }

    // 4. and the feature still works with the boundary installed.
    assert_line(&run.stdout, "probe: connect-public OK");
    assert_line(&run.stdout, "probe: connect-lan ");
    assert!(
        !run.stdout.contains("probe: connect-lan REACHABLE"),
        "the LAN must be dropped"
    );
    assert_line(&run.stdout, "probe: connect-host-loopback ");
    assert!(
        !run.stdout
            .contains("probe: connect-host-loopback REACHABLE"),
        "host loopback must not be reachable"
    );
}

#[test]
fn boundary_failure_never_starts_the_guest() {
    let _g = guard();
    let skipped_before = acos_test_support::skipped_count();
    acos_test_support::require_prereq(
        pasta_binary().is_some(),
        "spike net boundary (fail-closed)",
        "no pasta binary (set ACOS_SPIKE_NET_PASTA or build .cache/spike-net/bin/pasta)",
    );
    if acos_test_support::skipped_count() != skipped_before {
        return;
    }
    std::env::set_var("ACOS_SPIKE_NET", "1");
    std::env::set_var("ACOS_NET_ARM", "base");
    std::env::set_var("ACOS_SPIKE_NET_PASTA", pasta_binary().unwrap());
    let Some(root) = probe_root("boundary-fail") else {
        acos_test_support::require_prereq(
            false,
            "spike net boundary (fail-closed)",
            "no ART sysroot — run tools/art-sysroot.sh",
        );
        return;
    };
    let dir = scratch("boundary-fail-run");
    let probe = build_probe(&dir);
    let run = root.new_run_dir("boundary-fail").expect("run dir");
    let boundary_path = dir.join("net-boundary.txt");
    std::env::set_var("ACOS_SPIKE_NET_BOUNDARY", &boundary_path);
    let out_path = dir.join("probe.stdout");
    let out = std::fs::File::create(&out_path).expect("stdout file");
    let err_path = dir.join("probe.stderr");
    let err = std::fs::File::create(&err_path).expect("stderr file");
    let mut guest = guest_command(&root, run.path(), &probe, "net-guard");
    // No `iptables` on the shim's PATH: the boundary cannot be installed.
    guest.env("PATH", "/nonexistent");
    guest
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    let mut child = guest.spawn().expect("spawn pasta");
    let status =
        wait_with_timeout(&mut child, Duration::from_secs(60)).expect("the shim must not hang");
    let stdout = std::fs::read_to_string(&out_path).unwrap_or_default();
    let boundary = std::fs::read_to_string(&boundary_path).unwrap_or_default();
    eprintln!("--- boundary ---\n{boundary}\n--- probe stdout ---\n{stdout}");
    assert_eq!(
        status.code(),
        Some(acos_art::spike_net::BOUNDARY_FAIL_EXIT),
        "an unguarded session must fail closed"
    );
    assert!(
        !stdout.contains("probe: done"),
        "the guest must never run without the boundary"
    );
    assert_line(&boundary, "boundary=failed reason=no-iptables");
}
