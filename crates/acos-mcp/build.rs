//! Build fingerprint for the daemon/client identity (REL3D).
//!
//! A `acos serve` daemon must never answer a client built from different code
//! (MODA11Y: an older daemon answered `code 5 unsupported` for a service the
//! newer client implements). The runtime identity is
//!
//!   crate version + git sha + a hash of the workspace sources + a hash of the
//!   binary itself (computed once at process start, in `identity.rs`).
//!
//! The build-time constants come from the one shared implementation
//! (`build-support/fingerprint.rs`, also used by `crates/acos-webview` for the
//! WebView broker's build id), so every binary of one build agrees and any other
//! build differs.

include!("../../build-support/fingerprint.rs");

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
    emit_build_fingerprint(std::path::Path::new(&manifest));
}
