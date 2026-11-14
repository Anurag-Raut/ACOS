//! JVM dump routing for the build-only JVMs ACOS spawns (`javac`, `d8`'s
//! `java`).
//!
//! ## Why this module exists
//!
//! `javac` compiles the guest sources (`session-guest`, `launch-guest`,
//! render-app guest) and `d8` runs on a JVM too. Both inherit **ACOS's own
//! working directory**, so a JVM that dies leaves its dump files next to
//! whatever the agent ran from: 21 `replay_pid<pid>.log` files (HotSpot
//! compiler-replay data, a `javac` JVM's compile trace of
//! `com/sun/tools/javac/...`) once appeared in the ACOS checkout root, one per
//! cold compile, and `hs_err_pid<pid>.log` files land there for any fatal
//! error. That is repo noise for humans and a misleading signal for agents: the
//! dump names a *build tool*, not the app under test.
//!
//! ## What it does
//!
//! Every JVM ACOS starts is given an explicit, absolute dump location under the
//! ACOS scratch root (`<root>/.cache/art/jvm-dumps/`, next to the build cache),
//! so a dump is (a) never written into the caller's directory, (b) still
//! captured when a build tool really does crash, and (c) attributable to a run
//! by its path.
//!
//! The flags are `-J`-prefixed because the two entry points are `javac` and the
//! Android build-tools wrappers (`d8`), which forward initial `-J…` arguments to
//! the JVM. `%p` is expanded by the JVM to its own pid, so parallel compiles
//! never overwrite each other.

use std::path::PathBuf;

/// Directory that receives JVM fatal-error reports and replay data.
///
/// Sits beside the content-addressed build cache: `<root>/.cache/art/jvm-dumps`
/// where `<root>` is `$ACOS_HOME` in an installed layout, else the checkout.
#[must_use]
pub fn dump_dir() -> PathBuf {
    let cache = crate::build_cache::cache_root();
    let base = cache
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or(cache);
    base.join("jvm-dumps")
}

/// JVM arguments (`javac -J…` / `d8 -J…`) that route both of HotSpot's dump
/// files into [`dump_dir`]. Append them **before** the tool's own arguments:
/// the build-tools wrappers only extract `-J` flags from the front.
#[must_use]
pub fn dump_args() -> Vec<String> {
    let dir = dump_dir();
    let _ = std::fs::create_dir_all(&dir);
    let dir = dir.display();
    vec![
        format!("-J-XX:ErrorFile={dir}/hs_err_pid%p.log"),
        format!("-J-XX:ReplayDataFile={dir}/replay_pid%p.log"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_args_are_absolute_and_pid_scoped() {
        let args = dump_args();
        assert_eq!(args.len(), 2);
        assert!(args[0].starts_with("-J-XX:ErrorFile=/"), "{:?}", args[0]);
        assert!(args[0].ends_with("/hs_err_pid%p.log"), "{:?}", args[0]);
        assert!(args[1].starts_with("-J-XX:ReplayDataFile=/"), "{:?}", args[1]);
        assert!(args[1].ends_with("/replay_pid%p.log"), "{:?}", args[1]);
        // The dump directory is created so the JVM never has to fall back to the
        // working directory (a missing directory is a silent cwd dump).
        assert!(dump_dir().is_dir(), "{}", dump_dir().display());
        assert!(!dump_dir().starts_with("/tmp"), "{}", dump_dir().display());
    }
}
