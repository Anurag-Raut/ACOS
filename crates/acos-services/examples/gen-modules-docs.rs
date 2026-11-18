//! Print `docs/MODULES.md` from the module registry (ADR-0029 item 6).
//!
//! ```text
//! cargo run -p acos-services --example gen-modules-docs > docs/MODULES.md
//! ```
//!
//! `crates/acos-services/tests/modules_doc.rs` fails when the checked-in copy
//! differs, so this is the one way to change the document.

use acos_services::module::ModuleRegistry;

fn main() {
    println!("<!-- ACOS_MODULES_BEGIN -->");
    print!("{}", ModuleRegistry::standard().modules_markdown());
    println!("<!-- ACOS_MODULES_END -->");
}
