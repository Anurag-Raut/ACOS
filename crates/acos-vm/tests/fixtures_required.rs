//! CI gate for results/audit/HONESTY_AUDIT.md finding #11.
//!
//! Fixture-dependent differential tests print `SKIP` and pass when
//! `fixtures/build` (gitignored) or `.cache/boot` is absent, so a fresh clone /
//! CI run is green without exercising the M1/M2 evidence. CI sets
//! `ACOS_REQUIRE_FIXTURES=1`; this test then fails if the fixtures are missing.
//! Without the variable it is a no-op so developer checkouts still build.

use std::path::Path;

#[test]
fn required_fixtures_are_present_when_requested() {
    if std::env::var_os("ACOS_REQUIRE_FIXTURES").is_none() {
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for rel in ["fixtures/build/dexprogs", ".cache/boot/android-34"] {
        assert!(
            root.join(rel).exists(),
            "ACOS_REQUIRE_FIXTURES is set but {rel} is missing; CI must build the fixtures instead of skipping"
        );
    }
}
