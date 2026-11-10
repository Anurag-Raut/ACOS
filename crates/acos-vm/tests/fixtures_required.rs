//! CI gate for results/audit/HONESTY_AUDIT.md finding #11 and W67A finding #7.
//!
//! Fixture-dependent differential tests print `SKIP` and pass when
//! `fixtures/build` (gitignored), `.cache/boot` or an ART reference is absent,
//! so a fresh clone / CI run is green without exercising the M1/M2 evidence.
//! CI sets `ACOS_REQUIRE_FIXTURES=1`; every skipping test consults the shared
//! `acos_test_support::have_fixture` gate, and this test hard-fails if any
//! required artifact is missing. Without the variable it is a no-op so
//! developer checkouts still build.

use acos_test_support::require_all;

#[test]
fn required_fixtures_are_present_when_requested() {
    // The union of every artifact the skipping differential tests consult.
    require_all(&[
        "fixtures/build/dexprogs",
        "fixtures/build/apps/hello-views.apk",
        "fixtures/build/apps/hello-jni.apk",
        ".cache/boot/android-34",
        ".cache/boot/android-34/framework-res.apk",
        ".cache/boot/android-34/BOOTCLASSPATH",
        "fixtures/reference/hello-views/art-api34-x86_64/ui_initial.xml",
        "fixtures/reference/hello-views/art-api34-x86_64/lifecycle-summary.txt",
        "fixtures/reference/hello-jni/art-api34-x86_64/observed-texts-initial.txt",
    ]);
}
