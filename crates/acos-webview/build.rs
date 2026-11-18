//! Build fingerprint of the **broker** (WVSCOPE).
//!
//! A shared WebView broker outlives the session that started it, so a daemon
//! left over from another build can answer a newer gate with its own (old)
//! behaviour — the same class of bug REL3D fixed for the `acos serve` daemon
//! (`crates/acos-mcp/src/identity.rs`). The gate therefore sends its build id in
//! the attach handshake and refuses a broker that reports another one.
//!
//! The constants come from the one shared implementation
//! (`build-support/fingerprint.rs`), so the gate's binary and the broker binary
//! agree exactly when they were built from the same sources.

include!("../../build-support/fingerprint.rs");

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
    emit_build_fingerprint(std::path::Path::new(&manifest));
}
