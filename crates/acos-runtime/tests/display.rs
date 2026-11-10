//! Display-service acceptance: the real `IDisplayManager$Stub$Proxy` reads the
//! `DisplayInfo` / `OverlayProperties` replies ACOS serves.
//!
//! `acos-runtime::display_java` materialises the real Java objects and lets
//! `DisplayInfo.writeToParcel` emit the bytes, so this test proves the real
//! `DisplayInfo.CREATOR` (and `OverlayProperties.CREATOR`) can read exactly what
//! was written — the thing the hand-written prefix codec got wrong
//! (`RuntimeException: bad array lengths`). The Java case asserts the
//! hello-views reference geometry; the toolchain is only used to build it, and
//! the test skips loudly when it is absent.

mod common;

use common::{compile_case, run_ok};

#[test]
fn display_manager_reply_round_trips_through_the_real_java_creators() {
    let Some(dex) = compile_case(&["DisplayMain"]) else {
        eprintln!(
            "SKIP display_manager_reply_round_trips_through_the_real_java_creators: \
             javac/d8/android.jar unavailable"
        );
        return;
    };
    let Some(stdout) = run_ok(&dex, "DisplayMain", "DisplayMain") else {
        return;
    };
    assert!(
        stdout.lines().any(|l| l == "display-ok"),
        "DisplayMain did not reach the final assertion:\n{stdout}"
    );
    for needle in [
        "displayId=0",
        "logical=1080x2400",
        "app=1080x2209",
        "densityDpi=420",
        "mode=1/1080x2400@60.000004",
        "cutout=no-safe-insets",
        "overlay=fp16=false mixedColorSpaces=false",
        "missingDisplay=null",
    ] {
        assert!(stdout.contains(needle), "missing `{needle}` in:\n{stdout}");
    }
}
