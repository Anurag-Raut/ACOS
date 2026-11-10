//! CI gate for the fixture-honesty rule (HONESTY_AUDIT #11 / W94-A #1).
//!
//! Fixture-dependent tests print `SKIP` and pass when a gitignored artifact is
//! absent, so a fresh clone or CI run can be green without exercising the
//! evidence. Every such test calls `acos_test_support::have_fixture`, which
//! panics when `ACOS_REQUIRE_FIXTURES` is set; this test is the single explicit
//! gate that `tools/verify-all.sh` runs *after* building the fixtures, so the
//! suite fails loudly if the build step silently produced nothing.
//!
//! Without the variable it is a no-op so developer checkouts still build.

#[test]
fn required_fixtures_are_present_when_requested() {
    acos_test_support::require_all(&[
        "fixtures/build/dexprogs",
        "fixtures/build/apps/native-hello.apk",
        "fixtures/build/apps/service-provider.apk",
        ".cache/boot/android-34/BOOTCLASSPATH",
        "crates/acos-jni/tests/data/libacosgles.so",
    ]);
}
