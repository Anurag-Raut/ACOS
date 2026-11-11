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
    let seccomp_deferred = value_of(&args, "--seccomp-deferred").map(PathBuf::from);

    if action == "probe" {
        let scratch = rw
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("acos-sandbox-probe"));
        let mut profile = acos_sandbox::ConfineProfile::probe(scratch);
        if let Some(path) = &ro {
            profile.read_only_paths.push(path.clone());
        }
        if let Some(path) = &seccomp_deferred {
            profile.seccomp = acos_sandbox::SeccompMode::DeferredPreload(path.clone());
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
        if let Some(path) = &seccomp_deferred {
            profile.seccomp = acos_sandbox::SeccompMode::DeferredPreload(path.clone());
        }
        if let Err(e) = acos_sandbox::confine(&profile) {
            eprintln!("CONFINE_FAILED: {e}");
            return EXIT_SETUP_FAILED;
        }
    }

    match action.as_str() {
        "open_ro" => do_read(ro.map(|p| p.join("hello.txt"))),
        "open_outside" => do_read(outside.map(|p| p.join("secret.txt"))),
        // `realpath(3)`/`File.canonicalize0` of a granted path needs search
        // (`EXECUTE`) on every ancestor; of an outside path it must still be
        // denied (W120-A).
        "canonicalize_ro" => do_canonicalize(ro.map(|p| p.join("hello.txt"))),
        "canonicalize_outside" => do_canonicalize(outside.map(|p| p.join("secret.txt"))),
        // Traversal (`EXECUTE`) on an ancestor must not become directory listing
        // (`READ_DIR`) of that ancestor or its siblings (W120-A).
        "list_outside" => do_list(outside),
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
        // W67A #1/#2 escape surface: every one must return `EACCES` from the
        // seccomp filter (not a kernel `EPERM`), so `classify_seccomp` is used.
        "io_uring_setup" => do_io_uring_setup(),
        "userfaultfd" => do_userfaultfd(),
        "open_by_handle_at" => do_open_by_handle_at(),
        "name_to_handle_at" => do_name_to_handle_at(),
        "fsopen" => do_fsopen(),
        "move_mount" => do_move_mount(),
        "pidfd_open" => do_pidfd_open(),
        "kcmp" => do_kcmp(),
        "kill_other" => do_kill_other(),
        "setuid" => do_setuid(),
        "clone_userns" => do_clone_userns(),
        // Argument-filtered allow: signals to the own thread group still work.
        "tgkill_self" => do_tgkill_self(),
        "personality_ok" => do_personality_ok(),
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

/// Stricter classifier for the seccomp escape tests: only `EACCES` counts as a
/// denial, so a kernel-side `EPERM` (the syscall *reached the kernel*) is
/// reported as unexpected and fails the test.
fn classify_seccomp(error: &std::io::Error) -> i32 {
    match error.raw_os_error() {
        Some(code) if code == libc::EACCES => EXIT_DENIED,
        _ => EXIT_UNEXPECTED,
    }
}

fn do_io_uring_setup() -> i32 {
    let mut params = [0u8; 256];
    // SAFETY: `params` is a valid writable buffer the kernel may fill.
    let rc = unsafe { libc::syscall(libc::SYS_io_uring_setup, 8u32, params.as_mut_ptr()) };
    if rc >= 0 {
        // SAFETY: `rc` is a live ring fd.
        unsafe {
            libc::close(rc as i32);
        }
        EXIT_OK
    } else {
        classify_seccomp(&std::io::Error::last_os_error())
    }
}

fn do_userfaultfd() -> i32 {
    // SAFETY: `userfaultfd` takes an int flag word.
    let rc = unsafe { libc::syscall(libc::SYS_userfaultfd, 0u32) };
    if rc >= 0 {
        // SAFETY: `rc` is a live userfaultfd.
        unsafe {
            libc::close(rc as i32);
        }
        EXIT_OK
    } else {
        classify_seccomp(&std::io::Error::last_os_error())
    }
}

fn do_open_by_handle_at() -> i32 {
    let handle = [0u8; 64];
    // SAFETY: `handle` is a readable buffer; the kernel validates the fields.
    let rc =
        unsafe { libc::syscall(libc::SYS_open_by_handle_at, -1i32, handle.as_ptr(), 0i32) };
    if rc >= 0 {
        // SAFETY: `rc` is a live fd.
        unsafe {
            libc::close(rc as i32);
        }
        EXIT_OK
    } else {
        classify_seccomp(&std::io::Error::last_os_error())
    }
}

fn do_name_to_handle_at() -> i32 {
    let path = CString::new("/").expect("no NUL");
    let mut handle = [0u8; 64];
    let mut mount_id: i32 = 0;
    // SAFETY: all pointers are valid for the duration of the call.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_name_to_handle_at,
            libc::AT_FDCWD,
            path.as_ptr(),
            handle.as_mut_ptr(),
            &mut mount_id as *mut i32,
            0i32,
        )
    };
    if rc >= 0 {
        EXIT_OK
    } else {
        classify_seccomp(&std::io::Error::last_os_error())
    }
}

fn do_fsopen() -> i32 {
    let fstype = CString::new("tmpfs").expect("no NUL");
    // SAFETY: `fstype` is a valid C string.
    let rc = unsafe { libc::syscall(libc::SYS_fsopen, fstype.as_ptr(), 0u32) };
    if rc >= 0 {
        // SAFETY: `rc` is a live fs_context fd.
        unsafe {
            libc::close(rc as i32);
        }
        EXIT_OK
    } else {
        classify_seccomp(&std::io::Error::last_os_error())
    }
}

fn do_move_mount() -> i32 {
    let path = CString::new("/").expect("no NUL");
    // SAFETY: both paths are valid C strings; the fds/dirs are invalid on
    // purpose so a permitted call would fail harmlessly.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_move_mount,
            -1i32,
            path.as_ptr(),
            -1i32,
            path.as_ptr(),
            0u32,
        )
    };
    if rc >= 0 {
        EXIT_OK
    } else {
        classify_seccomp(&std::io::Error::last_os_error())
    }
}

fn do_pidfd_open() -> i32 {
    // SAFETY: scalar arguments only.
    let rc = unsafe { libc::syscall(libc::SYS_pidfd_open, 1u32, 0u32) };
    if rc >= 0 {
        // SAFETY: `rc` is a live pidfd.
        unsafe {
            libc::close(rc as i32);
        }
        EXIT_OK
    } else {
        classify_seccomp(&std::io::Error::last_os_error())
    }
}

fn do_kcmp() -> i32 {
    const KCMP_FILE: u32 = 0;
    // SAFETY: scalar arguments only; both pids are our own.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_kcmp,
            libc::getpid(),
            libc::getpid(),
            KCMP_FILE,
            0usize,
            0usize,
        )
    };
    if rc >= 0 {
        EXIT_OK
    } else {
        classify_seccomp(&std::io::Error::last_os_error())
    }
}

fn do_kill_other() -> i32 {
    // SAFETY: signal 0 performs only a permission check on pid 1.
    let rc = unsafe { libc::kill(1, 0) };
    if rc == 0 {
        EXIT_OK
    } else {
        classify_seccomp(&std::io::Error::last_os_error())
    }
}

fn do_setuid() -> i32 {
    // SAFETY: `setuid` takes a scalar uid.
    let rc = unsafe { libc::setuid(0) };
    if rc == 0 {
        EXIT_OK
    } else {
        classify_seccomp(&std::io::Error::last_os_error())
    }
}

fn do_clone_userns() -> i32 {
    // Fork-like `clone(CLONE_NEWUSER|SIGCHLD)` with a dedicated stack. If the
    // filter permits it, the child exits immediately; if it is correctly
    // denied, the syscall returns `EACCES` and no child exists (W67A #2).
    let mut stack = vec![0u8; 128 * 1024];
    // SAFETY: `stack` outlives the call and the child only touches this stack.
    let stack_top = unsafe { stack.as_mut_ptr().add(stack.len()) } as *mut libc::c_void;
    // SAFETY: the pointer arguments are either NULL or the live `stack_top`.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_clone,
            (libc::CLONE_NEWUSER | libc::SIGCHLD) as libc::c_ulong,
            stack_top,
            std::ptr::null_mut::<libc::c_int>(),
            std::ptr::null_mut::<libc::c_int>(),
            0usize,
        )
    };
    if rc < 0 {
        return classify_seccomp(&std::io::Error::last_os_error());
    }
    if rc == 0 {
        // Child: never continue into the parent's control flow.
        // SAFETY: `_exit` terminates the freshly cloned child.
        unsafe { libc::_exit(0) };
    }
    let mut status = 0;
    // SAFETY: `rc` is the child pid returned by clone.
    unsafe {
        libc::waitpid(rc as libc::pid_t, &mut status, 0);
    }
    EXIT_OK
}

fn do_tgkill_self() -> i32 {
    // SAFETY: `gettid` has no preconditions.
    let tid = unsafe { libc::syscall(libc::SYS_gettid) };
    // SAFETY: signal 0 to our own thread group is a permission check only.
    let rc = unsafe { libc::syscall(libc::SYS_tgkill, libc::getpid(), tid, 0u32) };
    if rc == 0 {
        EXIT_OK
    } else {
        EXIT_UNEXPECTED
    }
}

fn do_personality_ok() -> i32 {
    // SAFETY: personality(0) sets PER_LINUX and returns the previous value.
    let rc = unsafe { libc::syscall(libc::SYS_personality, 0u32) };
    if rc >= 0 {
        EXIT_OK
    } else {
        classify_seccomp(&std::io::Error::last_os_error())
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

fn do_canonicalize(path: Option<PathBuf>) -> i32 {
    let Some(path) = path else {
        return EXIT_UNEXPECTED;
    };
    match std::fs::canonicalize(&path) {
        Ok(_) => EXIT_OK,
        Err(e) => classify(&e),
    }
}

fn do_list(path: Option<PathBuf>) -> i32 {
    let Some(path) = path else {
        return EXIT_UNEXPECTED;
    };
    match std::fs::read_dir(&path) {
        Ok(entries) => {
            // Force the directory read so the denial surfaces here.
            for entry in entries {
                if entry.is_err() {
                    return EXIT_UNEXPECTED;
                }
            }
            EXIT_OK
        }
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
