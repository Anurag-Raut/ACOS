//! `docs/MODULES.md` must match the module registry exactly.
//!
//! ADR-0029 item 6 / root-cause fix D: the documentation an agent plans from is
//! generated from the registry (like `acos_capabilities` and the guide's limits
//! topic), not written by hand. An agent that reads "`location`: `scripted`" in
//! the doc has to get a module that really answers that, and a module added to
//! `ModuleRegistry::standard()` has to appear without anyone remembering to edit
//! a document.
//!
//! Regenerate with:
//!
//! ```text
//! cargo run -p acos-services --example gen-modules-docs > docs/MODULES.md
//! ```

use std::path::{Path, PathBuf};

use acos_services::module::{ModuleRegistry, TOMBSTONES};

const BEGIN: &str = "<!-- ACOS_MODULES_BEGIN -->";
const END: &str = "<!-- ACOS_MODULES_END -->";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn generated_block(doc: &str) -> Option<String> {
    let start = doc.find(BEGIN)? + BEGIN.len();
    let end = doc[start..].find(END)? + start;
    Some(doc[start..end].trim_matches('\n').to_string())
}

#[test]
fn docs_modules_md_matches_the_registry() {
    let path = repo_root().join("docs/MODULES.md");
    let doc = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let checked_in = generated_block(&doc).unwrap_or_else(|| {
        panic!(
            "{} is missing the `{BEGIN}` / `{END}` block",
            path.display()
        )
    });
    let expected = ModuleRegistry::standard().modules_markdown();
    assert_eq!(
        checked_in.trim_end(),
        expected.trim_end(),
        "docs/MODULES.md drifted from the module registry; regenerate it with \
         `cargo run -p acos-services --example gen-modules-docs > docs/MODULES.md`"
    );
}

/// The document is not just present: every registered module and every gap name
/// is in it, with its profiles and attach mode.
#[test]
fn docs_modules_md_lists_every_module_and_gap() {
    let path = repo_root().join("docs/MODULES.md");
    let doc = std::fs::read_to_string(&path).expect("docs/MODULES.md");
    let registry = ModuleRegistry::standard();
    for manifest in registry.manifests() {
        let row_start = format!("| `{}` |", manifest.id);
        let row = doc
            .lines()
            .find(|line| line.starts_with(&row_start))
            .unwrap_or_else(|| panic!("module {} has no row in docs/MODULES.md", manifest.id));
        for profile in manifest.profiles_with_off() {
            assert!(
                row.contains(&format!("`{profile}`")),
                "module {}'s row omits profile {profile}: {row}",
                manifest.id
            );
        }
        assert!(
            row.contains(&format!("`{}`", manifest.attach.as_str())),
            "module {}'s row omits its attach mode: {row}",
            manifest.id
        );
    }
    for tombstone in TOMBSTONES {
        assert!(
            doc.contains(&format!(
                "| `{}` | `{}` |",
                tombstone.name, tombstone.module
            )),
            "gap name {} is missing from docs/MODULES.md",
            tombstone.name
        );
    }
}
