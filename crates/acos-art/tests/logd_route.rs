//! ZPROD7: a forked guest's log stream goes to *its* session's sink.
//!
//! The exec path starts a launcher whose `logd` sink (`Logd::start`) is the
//! guest's `/dev/socket/logdw`, so `{"cmd":"logs"}` and the session's logcat
//! artifact see the app's entries. A production-zygote child is different: the
//! **keeper** resolves that socket before it forks, so the child inherited the
//! connected descriptor and its entries went to the keeper's buffer. Measured as
//! the flag-on `form-validation` gate failing at step 5 `assert_log` with an
//! empty session log buffer, while the lines were in the keeper's.
//!
//! These tests drive the *shipped* shim (`acos_art::SHIM_SOURCE`, i.e.
//! `crates/acos-art/shim/artshim.c`) in real processes with a real `fork()` in
//! the middle, so the inherited descriptor is a real inherited descriptor.
//!
//! A zygote child resolves the *same* absolute path (`/dev/socket/logdw`) to a
//! *different* socket inode, because its `/dev/socket` is its own session's bind
//! mount. The test reproduces exactly that shape with a **relative** sink path
//! and a per-process working directory: same path string, different resolution,
//! no shared mutable state, so two writers can run at the same time.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use acos_art::{Logd, SHIM_SOURCE};

/// Short on purpose: the paths below include a `logdw` socket, and an `AF_UNIX`
/// `sun_path` is 108 bytes including its NUL (the same wall ZPROD5 measured on
/// the session directory).
fn scratch(name: &str) -> PathBuf {
    // A unix socket path must fit SUN_LEN (108 bytes including the NUL), i.e. at
    // most 107 characters. `CARGO_MANIFEST_DIR` is absolute but the suffix below
    // carries `..` segments, and a git worktree under `.t3/worktrees/<repo>/<id>/`
    // adds ~40 characters, so the *resolved* path overflows the limit and the
    // sink cannot bind. Resolve first, then fall back to a short hashed name
    // (still inside this checkout's `.cache`, never /tmp).
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cache/z7")
        .join(format!("{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch dir");
    let mut dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    // `/k/logdw` and `/s0/logdw` are the longest suffixes the tests add.
    let budget = 107 - "/k/logdw".len();
    if dir.as_os_str().len() > budget {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in format!("{}-{name}", std::process::id()).bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cache")
            .join(format!("z7-{hash:016x}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    }
    let sink = dir.join("k/logdw");
    assert!(
        sink.as_os_str().len() <= 107,
        "the test's own sink path {} is {} characters; SUN_LEN allows 107",
        sink.display(),
        sink.as_os_str().len()
    );
    dir
}

/// Build the production shim with the sink path relative (the test's stand-in
/// for a per-mount `/dev/socket`).
///
/// Same compiler and flags as [`acos_art::build_shim`]; `-D` is the only
/// difference. The source that is compiled *is* the shipped source, so the
/// logger under test is the logger that ships.
fn shim_with_relative_sink(dir: &Path) -> PathBuf {
    let src = dir.join("s.c");
    let out = dir.join("l.so");
    fs::write(&src, SHIM_SOURCE).expect("write the shipped shim source");
    let cc = std::env::var("CC").unwrap_or_else(|_| "clang".into());
    let st = Command::new(&cc)
        .args([
            "-nostdlib",
            "-ffreestanding",
            "-fno-builtin",
            "-fPIC",
            "-shared",
            "-O2",
            "-Wall",
            "-DACOSART_LOGD_SOCK=\"logdw\"",
            "-o",
        ])
        .arg(&out)
        .arg(&src)
        .status()
        .expect("run the C compiler");
    assert!(st.success(), "{cc} failed to build the shim");
    let mode = fs::metadata(&out)
        .expect("shim metadata")
        .permissions()
        .mode();
    assert!(mode & 0o400 != 0, "the shim must be readable: {mode:o}");
    out
}

/// The guest-side writer of one session.
///
/// `keeper_dir` is the process's `/dev/socket` before the fork, `session_dir`
/// the one its child gets. One real `fork()`; the child logs after it, so its
/// entries can only reach its own sink if the shim re-resolves the path in the
/// new process.
fn writer(dir: &Path) -> PathBuf {
    let src = dir.join("p.c");
    let exe = dir.join("p");
    fs::write(
        &src,
        "#include <stdio.h>\n\
         #include <stdlib.h>\n\
         #include <unistd.h>\n\
         #include <sys/wait.h>\n\
         #include <dlfcn.h>\n\
         /* Resolved once, before any `fork()`, so the child never enters the\n\
            dynamic loader (it is not fork-safe in general). */\n\
         typedef int (*logfn)(int, const char *, const char *);\n\
         static logfn say_fn;\n\
         static int say(const char *tag, const char *msg) {\n\
           if (!say_fn) {\n\
             say_fn = (logfn)dlsym(RTLD_DEFAULT, \"__android_log_write\");\n\
             if (!say_fn) return -1;\n\
           }\n\
           say_fn(4, tag, msg); /* returns 1 on success, bionic-style */\n\
           return 0;\n\
         }\n\
         int main(int argc, char **argv) {\n\
           if (argc < 5) return 2;\n\
           /* argv: 1=keeper socket dir, 2=session socket dir, 3=tag,\n\
             4=number of child lines */\n\
           const char *keeper = argv[1];\n\
           const char *session = argv[2];\n\
           const char *tag = argv[3];\n\
           int lines = atoi(argv[4]);\n\
           if (chdir(keeper) != 0) return 3;\n\
           if (say(tag, \"keeper-line\") != 0) return 91;\n\
           pid_t pid = fork();\n\
           if (pid < 0) return 4;\n\
           if (pid == 0) {\n\
             /* The child's own mount: the same path, a different socket. */\n\
             if (chdir(session) != 0) _exit(5);\n\
             for (int i = 0; i < lines; i++) {\n\
               char msg[64];\n\
               snprintf(msg, sizeof msg, \"child-line-%d\", i);\n\
               if (say(tag, msg) != 0) _exit(92);\n\
             }\n\
             _exit(0);\n\
           }\n\
           int st = 0;\n\
           if (waitpid(pid, &st, 0) != pid) return 6;\n\
           if (!WIFEXITED(st) || WEXITSTATUS(st) != 0) return 7;\n\
           return 0;\n\
         }\n",
    )
    .expect("write the probe");
    let cc = std::env::var("CC").unwrap_or_else(|_| "clang".into());
    let st = Command::new(&cc)
        .args(["-x", "c", "-O0", "-ldl", "-o"])
        .arg(&exe)
        .arg(&src)
        .status()
        .expect("run cc for the probe");
    assert!(st.success(), "{cc} failed to build the logd probe");
    exe
}

/// Collect what a sink holds until it has at least `want` entries (or the
/// deadline passes), so the assertion reports what really arrived.
fn wait_for(sink: &Logd, want: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let got: Vec<String> = sink.snapshot().iter().map(|e| e.message.clone()).collect();
        if got.len() >= want || Instant::now() > deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The keeper's sink plus one sink per session, laid out the way a zygote
/// session is: each sink lives in that session's own socket directory.
struct Sinks {
    keeper_dir: PathBuf,
    keeper: Logd,
    /// `(socket dir, sink)` per session, in tag order.
    sessions: Vec<(PathBuf, Logd)>,
}

fn sinks(root: &Path, n: usize) -> Sinks {
    let keeper_dir = root.join("k");
    fs::create_dir_all(&keeper_dir).expect("keeper socket dir");
    let keeper = Logd::start(&keeper_dir).expect("keeper sink");
    let mut sessions = Vec::new();
    for k in 0..n {
        let dir = root.join(format!("s{k}"));
        fs::create_dir_all(&dir).expect("session socket dir");
        sessions.push((dir.clone(), Logd::start(&dir).expect("session sink")));
    }
    Sinks {
        keeper_dir,
        keeper,
        sessions,
    }
}

/// The acceptance shape: two concurrent zygote children, one sink each.
#[test]
fn two_concurrent_forked_children_each_log_only_into_their_own_session() {
    let root = scratch("two-sessions");
    let s = sinks(&root, 2);
    let shim = shim_with_relative_sink(&root);
    let probe = writer(&root);

    // Start both before waiting on either: they are concurrent, as two zygote
    // sessions are. They share the same sink *path* (as two sessions share
    // `/dev/socket`) and never a sink.
    let mut children = Vec::new();
    for (k, (dir, _)) in s.sessions.iter().enumerate() {
        let child = Command::new(&probe)
            .args([
                s.keeper_dir.to_str().expect("utf-8 keeper"),
                dir.to_str().expect("utf-8 session"),
                &format!("s{k}"),
                "3",
            ])
            .env("LD_PRELOAD", &shim)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the forked writer");
        children.push(child);
    }
    for child in children {
        let out = child.wait_with_output().expect("writer exits");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "the writer must exit 0; got {:?}, stderr: {stderr}",
            out.status.code()
        );
        assert!(
            !stderr.contains("[art]"),
            "every entry must travel as a datagram to a sink, never the stderr \
             fallback: {stderr}"
        );
    }

    // The pre-fork lines (one per writer) are the keeper's and nothing else is.
    // This is the half the bug got wrong: it held the children's lines.
    let keeper = wait_for(&s.keeper, 2);
    assert_eq!(
        keeper,
        vec!["keeper-line".to_string(); 2],
        "only the pre-fork entries belong to the keeper"
    );
    for (k, (_, sink)) in s.sessions.iter().enumerate() {
        let got = wait_for(sink, 3);
        assert_eq!(
            got,
            vec![
                "child-line-0".to_string(),
                "child-line-1".to_string(),
                "child-line-2".to_string()
            ],
            "session {k} must hold exactly its own child's three lines"
        );
        let tags: Vec<String> = sink.snapshot().iter().map(|e| e.tag.clone()).collect();
        assert!(
            tags.iter().all(|t| t == &format!("s{k}")),
            "session {k} carries a foreign session's entry: {tags:?}"
        );
    }
}

/// A sink that is missing when an entry is written must not silence the stream
/// for the rest of the session: the connect is retried, not cached as a
/// permanent failure (the launcher binds the session's sink before the fork, but
/// a reset re-binds it).
#[test]
fn an_entry_written_before_the_sink_exists_does_not_silence_the_stream() {
    let root = scratch("late-sink");
    let mut s = sinks(&root, 1);
    let (session_dir, sink) = s.sessions.pop().expect("one session");
    drop(sink);
    let shim = shim_with_relative_sink(&root);
    let probe = writer(&root);

    let out = Command::new(&probe)
        .args([
            s.keeper_dir.to_str().expect("utf-8 keeper"),
            session_dir.to_str().expect("utf-8 session"),
            "s0",
            "3",
        ])
        .env("LD_PRELOAD", &shim)
        .output()
        .expect("writer exits");
    assert!(
        out.status.success(),
        "writer exits 0: {:?}",
        out.status.code()
    );
    let keeper = wait_for(&s.keeper, 1);
    assert_eq!(keeper, vec!["keeper-line".to_string()]);

    // Re-bind the session sink (the new launcher) and write through the same
    // path again: the earlier failure must not be sticky.
    let rebound = Logd::start(&session_dir).expect("rebound session sink");
    let out = Command::new(&probe)
        .args([
            s.keeper_dir.to_str().expect("utf-8 keeper"),
            session_dir.to_str().expect("utf-8 session"),
            "s0",
            "1",
        ])
        .env("LD_PRELOAD", &shim)
        .output()
        .expect("second writer exits");
    assert!(
        out.status.success(),
        "a failed connect must be retried, not cached: {:?} / {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        wait_for(&rebound, 1),
        vec!["child-line-0".to_string()],
        "the rebound sink receives the next entry"
    );
}

/// Contract guard: the production build must keep the guest path, and the
/// reconnect rule must stay. The path is the *only* per-session fact the logger
/// relies on, so changing it changes the isolation guarantee.
#[test]
fn the_default_sink_path_is_the_guest_socket() {
    assert!(
        SHIM_SOURCE.contains("#define ACOSART_LOGD_SOCK \"/dev/socket/logdw\""),
        "the shipped default sink path must stay /dev/socket/logdw"
    );
    assert!(
        SHIM_SOURCE.contains("SYS_getpid"),
        "the fork-child reconnect rule must stay in the shipped shim"
    );
}
