//! Engine handshake without the sandbox: proves the pinned binary speaks
//! DevTools over `--remote-debugging-pipe` on this host, and keeps the failure
//! mode readable (the engine's own stderr tail) when it does not.
//!
//! Guard: same as `engine_gate` (`ACOS_WEBVIEW_TEST=1`, run under
//! `run-exclusive` + `guarded.sh`); it starts a real Chromium for ~2 s.
//!
//! ```text
//! tools/art-runtime/run-exclusive.sh --task WVB -- tools/guarded.sh --mem 4G --time 180 -- \
//!   env ACOS_WEBVIEW_TEST=1 CARGO_TARGET_DIR=$PWD/.cache/wvb-target \
//!   cargo test -p acos-webview --test engine_bare -- --nocapture --test-threads=1
//! ```

use std::process::Command;
use std::time::Duration;

use acos_webview::cdp::CdpClient;
use acos_webview::engine::EngineCache;
use acos_webview::sandbox::engine_args;
use acos_webview::sandbox::ProfileDir;
use serde_json::json;

#[test]
fn engine_handshake_over_the_devtools_pipe() {
    if std::env::var("ACOS_WEBVIEW_TEST").as_deref() != Ok("1") {
        eprintln!("SKIP engine_handshake: set ACOS_WEBVIEW_TEST=1 (heavy: starts a Chromium)");
        return;
    }
    let cache = EngineCache::default_cache();
    let bin = cache.ensure().expect("engine install");
    let profile_root = cache.root().join("run").join("bare-profiles");
    let profile = ProfileDir::create(&profile_root).expect("profile");
    let mut cmd = Command::new(&bin);
    cmd.args(engine_args(profile.path()));
    let cdp = CdpClient::spawn(cmd).unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));
    let version = match cdp.call(
        "Browser.getVersion",
        json!({}),
        None,
        Duration::from_secs(30),
    ) {
        Ok(v) => v,
        Err(e) => panic!(
            "Browser.getVersion failed: {e}\nengine argv: {:?}\nengine stderr (tail):\n{}",
            cdp.argv,
            cdp.stderr_tail()
        ),
    };
    eprintln!("engine: {version}");
    assert!(
        version["product"]
            .as_str()
            .unwrap_or("")
            .starts_with("HeadlessChrome/"),
        "unexpected engine product: {version}"
    );
    assert_eq!(version["protocolVersion"], json!("1.3"));
    cdp.shutdown();
    let _ = std::fs::remove_dir_all(profile_root);
}
