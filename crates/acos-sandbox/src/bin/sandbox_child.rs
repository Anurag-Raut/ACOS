//! Hostile native test program for the ACOS confinement layer.
//!
//! This is **not** a product binary and never runs untrusted APK code. It is
//! invoked by `tests/confine.rs` in a fresh child process: it optionally
//! confines itself with [`acos_sandbox::confine`], performs exactly one
//! requested action, and exits with a code the parent can assert on:
//!
//! * `0`  — the action succeeded (a permitted action);
//! * `10` — the action was denied by Landlock or seccomp (`EACCES`/`EPERM`);
//! * `20` — confinement itself failed;
//! * `30` — the action failed for an unexpected reason.
//!
//! `--action probe` prints the full [`ConfinementReport`] as JSON instead.

use std::ffi::CString;
use std::path::PathBuf;

const EXIT_OK: i32 = 0;
const EXIT_DENIED: i32 = 10;
const EXIT_SETUP_FAILED: i32 = 20;
const EXIT_UNEXPECTED: i32 = 30;

fn main() {
    std::process::exit(run());
}

fn value_of(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    let Some(action) = value_of(&args, "--action") else {
        eprintln!(
            "usage: sandbox_child --action <name> [--ro DIR] [--rw DIR] \
             [--outside DIR] [--no-confinement]"
        );
        return 2;
    };
    let no_confinement = args.iter().any(|a| a == "--no-confinement");
    let ro = value_of(&args, "--ro").map(PathBuf::from);
    let rw = value_of(&args, "--rw").map(PathBuf::from);
    let outside = value_of(&args, "--outside").map(PathBuf::from);

    if action == "probe" {
        let scratch = rw
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("acos-sandbox-probe"));
        let mut profile = acos_sandbox::ConfineProfile::probe(scratch);
        if let Some(path) = &ro {
            profile.read_only_paths.push(path.clone());
        }
        return match acos_sandbox::confine(&profile) {
            Ok(report) => {
                println!("{}", serde_json::to_string(&report).unwrap_or_default());
                EXIT_OK
            }
            Err(e) => {
                eprintln!("CONFINE_FAILED: {e}");
                EXIT_SETUP_FAILED
            }
        };
    }

    if !no_confinement {
        let mut profile = acos_sandbox::ConfineProfile::default();
        if let Some(path) = &ro {
            profile.read_only_paths.push(path.clone());
        }
        if let Some(path) = &rw {
            profile.read_write_paths.push(path.clone());
        }
        if let Err(e) = acos_sandbox::confine(&profile) {
            eprintln!("CONFINE_FAILED: {e}");
            return EXIT_SETUP_FAILED;
        }
    }

    match action.as_str() {
        "open_ro" => do_read(ro.map(|p| p.join("hello.txt"))),
        "open_outside" => do_read(outside.map(|p| p.join("secret.txt"))),
        "write_data" => do_write(rw.map(|p| p.join("out.txt"))),
        "write_ro" => do_write(ro.map(|p| p.join("nope.txt"))),
        "write_outside" => do_write(outside.map(|p| p.join("nope.txt"))),
        "threads" => do_threads(),
        "mmap_exec" => do_mmap_exec(),
        "futex" => do_futex(),
        "exec" => do_exec(),
        "ptrace" => do_ptrace(),
        "socket_inet" => do_socket_inet(),
        "process_vm" => do_process_vm(),
        other => {
            eprintln!("unknown action {other:?}");
            2
        }
    }
}

fn classify(error: &std::io::Error) -> i32 {
    match error.raw_os_error() {
        Some(code) if code == libc::EACCES || code == libc::EPERM => EXIT_DENIED,
        _ => EXIT_UNEXPECTED,
    }
}

fn do_read(path: Option<PathBuf>) -> i32 {
    let Some(path) = path else {
        return EXIT_UNEXPECTED;
    };
    match std::fs::read_to_string(&path) {
        Ok(_) => EXIT_OK,
        Err(e) => classify(&e),
    }
}

fn do_write(path: Option<PathBuf>) -> i32 {
    let Some(path) = path else {
        return EXIT_UNEXPECTED;
    };
    match std::fs::write(&path, b"acos\n") {
        Ok(_) => EXIT_OK,
        Err(e) => classify(&e),
    }
}

fn do_threads() -> i32 {
    let mut handles = Vec::new();
    for i in 0..4u64 {
        handles.push(std::thread::spawn(move || i * i));
    }
    for handle in handles {
        if handle.join().is_err() {
            return EXIT_UNEXPECTED;
        }
    }
    EXIT_OK
}

fn do_mmap_exec() -> i32 {
    // SAFETY: anonymous private mapping; we write one byte then unmap it.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return EXIT_UNEXPECTED;
    }
    // SAFETY: `ptr` is a writable mapping of one page.
    unsafe {
        *(ptr as *mut u8) = 0xc3;
        libc::munmap(ptr, 4096);
    }
    EXIT_OK
}

fn do_futex() -> i32 {
    let mut word: u32 = 0;
    // SAFETY: futex operates on the aligned 4-byte word we own.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_futex,
            &mut word as *mut u32,
            libc::FUTEX_WAKE,
            1u32,
            0u64,
            0u64,
            0u64,
        )
    };
    if rc < 0 {
        EXIT_UNEXPECTED
    } else {
        EXIT_OK
    }
}

fn do_exec() -> i32 {
    let path = CString::new("/bin/true").expect("no NUL");
    let argv = [path.as_ptr(), std::ptr::null()];
    // SAFETY: `path`/`argv` are valid NUL-terminated C values for execve. If
    // seccomp permits it the process is replaced by /bin/true; otherwise the
    // call returns and we classify the error.
    unsafe {
        libc::execve(path.as_ptr(), argv.as_ptr(), std::ptr::null());
    }
    classify(&std::io::Error::last_os_error())
}

fn do_ptrace() -> i32 {
    // SAFETY: PTRACE_TRACEME is the ptrace request that touches no memory.
    let rc = unsafe { libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) };
    if rc == 0 {
        EXIT_OK
    } else {
        classify(&std::io::Error::last_os_error())
    }
}

fn do_socket_inet() -> i32 {
    // SAFETY: `socket` takes scalar arguments.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if fd >= 0 {
        // SAFETY: `fd` is live and owned here.
        unsafe {
            libc::close(fd);
        }
        EXIT_OK
    } else {
        classify(&std::io::Error::last_os_error())
    }
}

fn do_process_vm() -> i32 {
    let mut buf = [0u8; 8];
    let local = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: 8,
    };
    let remote = libc::iovec {
        iov_base: buf.as_ptr() as *mut libc::c_void,
        iov_len: 8,
    };
    // SAFETY: `local`/`remote` describe our own buffers; the kernel validates
    // them. The syscall is denied by seccomp before it runs.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_process_vm_readv,
            libc::getpid(),
            &local,
            1usize,
            &remote,
            1usize,
            0usize,
        )
    };
    if rc >= 0 {
        EXIT_OK
    } else {
        classify(&std::io::Error::last_os_error())
    }
}
