//! Java-level acceptance for `android.system.OsConstants` (W25-B).
//!
//! Compiles `tests/java/OsConstantsMain.java` with the host `javac`/`d8`
//! (build tools only, never the execution path) and runs it on the real
//! Android 14 core library. The expected values are a byte-for-byte capture
//! from the API-34 `x86_64` ART oracle (`tools/oracle/emulator.sh`, reflection
//! dump — see `results/vm/OS_NATIVES_AUDIT.md`), so the table, the framework's
//! `errnoName`/`gaiName` consumers, and the file/stat/pipe/scatter-gather
//! round trips are pinned against real ART rather than a hand-written list.
//!
//! Two outputs intentionally differ from ART because they describe the
//! sandbox, not the shared ABI; they are asserted separately below:
//!
//! * `umask` — the sandbox uses a fixed `0022`; ART inherits the shell's umask;
//! * `environ_empty` — the guest sees the fixed, Android-like VM-owned
//!   environment table (non-empty), never the host process environment
//!   (`HONESTY_AUDIT.md` #5).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_vm::{CoreLib, Vm, VmError, VmOptions};

/// The complete stdout of `OsConstantsMain` on the API-34 `x86_64` ART oracle,
/// captured 2026-11-10 with:
///
/// ```text
/// CLASSPATH=/data/local/tmp/oscmain.dex app_process /system/bin \
///     OsConstantsMain /data/local/tmp/osconst-oracle
/// ```
const ART_REFERENCE: &str = "\
O_RDONLY=0
O_WRONLY=1
O_RDWR=2
O_CREAT=64
O_APPEND=1024
O_CLOEXEC=524288
EACCES=13
ENOENT=2
EEXIST=17
S_IFMT=61440
S_IFDIR=16384
S_IFREG=32768
AF_INET=2
AF_INET6=10
SOCK_STREAM=1
SOCK_DGRAM=2
IPPROTO_TCP=6
PROT_READ=1
PROT_WRITE=2
POLLIN=1
POLLOUT=4
SIGKILL=9
SIGTERM=15
SIGRTMIN=41
SIGRTMAX=64
MAP_PRIVATE=2
SEEK_SET=0
SEEK_END=2
_SC_PAGESIZE=39
_SC_PAGE_SIZE=40
_SC_CLK_TCK=6
_SC_OPEN_MAX=11
AI_ADDRCONFIG=1024
EAI_NONAME=8
NI_NUMERICHOST=2
errnoName_13=EACCES
errnoName_2=ENOENT
gaiName_8=EAI_NONAME
S_ISDIR_16384=true
S_ISDIR_32768=false
field_count=538
hash=7342789864980853825
file_same=true
file_len=7
stat_type_dir=false
stat_type_reg=true
stat_size=7
lstat_is_reg=true
access_f_ok=true
access_r_ok=true
access_missing_errno=2
pipe_written=3
pipe_read=3
pipe_same=true
sysconf_pagesize=4096
sysconf_clk_tck=100
strsignal_9=Killed
gai_strerror_8=hostname nor servname provided, or not known
writev=5
readv=5
v_same=true
socketpair_read=2
socketpair_same=true
memfd_read=3
memfd_same=true
fallocate_size=16
done
";

/// Keys whose ACOS value is a deliberate sandbox choice, not the ART value.
const SANDBOX_DIVERGENT: &[&str] = &["umask", "environ_empty"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Compile the Java case to a DEX. Returns `None` (skip loudly) when the host
/// build tools are unavailable.
fn compile_case(name: &str) -> Option<PathBuf> {
    let javac = which("javac")?;
    let sdk = std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .unwrap_or_else(|_| "/home/Qui/Android/Sdk".to_string());
    let d8 = PathBuf::from(format!("{sdk}/build-tools/35.0.0/d8"));
    let android_jar = PathBuf::from(format!("{sdk}/platforms/android-34/android.jar"));
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    let root = repo_root();
    let out = root.join("target/tmp/acos-os-constants").join(name);
    let dex = out.join("classes.dex");
    let src = root
        .join("crates/acos-vm/tests/java")
        .join(format!("{name}.java"));
    if !src.is_file() {
        return None;
    }
    // Always recompile: the Java source changes with the natives it exercises.
    let classes = out.join("classes");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&classes).ok()?;
    let ok = Command::new(&javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&android_jar)
        .args(["-d"])
        .arg(&classes)
        .arg(&src)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let class_file = classes.join(format!("{name}.class"));
    if !class_file.is_file() {
        return None;
    }
    let ok = Command::new(&d8)
        .arg("--lib")
        .arg(&android_jar)
        .args(["--min-api", "26", "--output"])
        .arg(&out)
        .arg(&class_file)
        .status()
        .ok()?
        .success();
    if !ok || !dex.is_file() {
        return None;
    }
    Some(dex)
}

fn parse_stdout(stdout: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

#[test]
fn os_constants_on_the_real_framework() {
    let Some(dex) = compile_case("OsConstantsMain") else {
        eprintln!("SKIP os_constants_on_the_real_framework: javac/d8/android.jar unavailable");
        return;
    };
    let options = VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    };
    let mut vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => {
            eprintln!("SKIP os_constants_on_the_real_framework: {m}");
            return;
        }
        Err(e) => panic!("Vm::new failed: {e}"),
    };
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let outcome = vm.run("OsConstantsMain", &[]);
    assert_eq!(
        outcome.exit_code, 0,
        "run failed: vm_error={:?} stderr={}\nstdout so far:\n{}",
        outcome.vm_error, outcome.stderr, outcome.stdout
    );
    assert!(outcome.vm_error.is_none(), "{:?}", outcome.vm_error);

    let got = parse_stdout(&outcome.stdout);
    let want = parse_stdout(ART_REFERENCE);
    assert!(
        !want.is_empty(),
        "embedded ART reference did not parse"
    );

    // Every captured key must match real ART, except the two sandbox choices.
    for (k, v) in &want {
        if SANDBOX_DIVERGENT.contains(&k.as_str()) {
            continue;
        }
        assert_eq!(
            got.get(k),
            Some(v),
            "`{k}` differs from the ART capture: got {:?}, want {:?}\nfull stdout:\n{}",
            got.get(k),
            Some(v),
            outcome.stdout
        );
    }
    // No unexpected/omitted keys: the program is deterministic.
    for k in got.keys() {
        if SANDBOX_DIVERGENT.contains(&k.as_str()) {
            continue;
        }
        assert!(
            want.contains_key(k),
            "`{k}` was not part of the ART reference"
        );
    }

    // The sandbox-specific values, with the honest reason recorded.
    assert_eq!(
        got.get("umask").map(String::as_str),
        Some("18"),
        "the ACOS sandbox uses a fixed umask of 0022 (decimal 18) (ART inherits the shell's)"
    );
    assert_eq!(
        got.get("environ_empty").map(String::as_str),
        Some("false"),
        "the guest sees the fixed Android-like table, not the host environment"
    );

    // Spot-check the framework-critical values in their raw form as well, so a
    // regression names the constant instead of only the checksum.
    assert_eq!(got.get("O_CREAT").map(String::as_str), Some("64"));
    assert_eq!(got.get("EACCES").map(String::as_str), Some("13"));
    assert_eq!(got.get("ENOENT").map(String::as_str), Some("2"));
    assert_eq!(got.get("S_IFMT").map(String::as_str), Some("61440"));
    assert_eq!(got.get("AF_INET").map(String::as_str), Some("2"));
    assert_eq!(got.get("SOCK_STREAM").map(String::as_str), Some("1"));
    assert_eq!(got.get("PROT_READ").map(String::as_str), Some("1"));
    assert_eq!(got.get("POLLIN").map(String::as_str), Some("1"));
    assert_eq!(got.get("SIGKILL").map(String::as_str), Some("9"));
    assert_eq!(got.get("_SC_PAGESIZE").map(String::as_str), Some("39"));
    // The fields the old partial table got wrong (glibc AI/NI/EAI + SIGRTMIN).
    assert_eq!(got.get("AI_ADDRCONFIG").map(String::as_str), Some("1024"));
    assert_eq!(got.get("EAI_NONAME").map(String::as_str), Some("8"));
    assert_eq!(got.get("NI_NUMERICHOST").map(String::as_str), Some("2"));
    assert_eq!(got.get("SIGRTMIN").map(String::as_str), Some("41"));
    assert_eq!(got.get("field_count").map(String::as_str), Some("538"));
}
