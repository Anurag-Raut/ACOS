//! T-38 follow-up acceptance: the real `IInputMethodManager$Stub$Proxy` and
//! `IClipboard$Stub$Proxy` read the Java-materialised IME/clipboard replies.
//!
//! `acos-runtime::ime_java` builds the real `InputBindResult` (using the
//! framework's own `NO_IME` constant) and `clipboard_java` stores/re-emits the
//! real `ClipData` bytes, so this test proves the real framework `CREATOR`s and
//! `$Stub$Proxy`s can read exactly what the real `writeToParcel` wrote. The Java
//! case also checks the `IOnPrimaryClipChangedListener` oneway delivery.
//!
//! The SDK toolchain is only used to build the case; when it (or the
//! boot-classpath cache) is absent the test skips loudly.

mod common;

use common::{compile_case, run_ok};

#[test]
fn ime_and_clipboard_replies_round_trip_through_real_java_proxies() {
    let Some(dex) = compile_case(&["ImeClipboardMain"]) else {
        acos_test_support::require_prereq(false, "ime_and_clipboard_replies_round_trip_through_real_java_proxies", "ime_and_clipboard_replies_round_trip_through_real_java_proxies: javac/d8/android.jar unavailable");
        return;
    };
    let Some(stdout) = run_ok(&dex, "ImeClipboardMain", "ImeClipboardMain") else {
        return;
    };
    assert!(
        stdout.lines().any(|l| l == "ime-clipboard-ok"),
        "ImeClipboardMain did not reach the final assertion:\n{stdout}"
    );
    for needle in [
        "addClient=ok",
        "ime-bind-ok",
        "imeList=empty",
        "hasPrimaryClip=true",
        "clipText=hello",
        "clipLabel=l",
        "clipboard-ok",
        "user=0/ACOS",
        "user-ok",
    ] {
        assert!(stdout.contains(needle), "missing `{needle}` in:\n{stdout}");
    }
}
