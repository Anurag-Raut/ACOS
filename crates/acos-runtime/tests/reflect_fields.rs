//! Reflection field-slot regression against the real Android 14 core library.
//!
//! `Field.artFieldIndex` is a declaration index while the storage slot is
//! `FieldDef.offset`: a `long`/`double` takes two slots, so a static field that
//! follows a wide field used to read the wrong slot. That is the
//! `Settings.Global` NPE that blocked the M2 launch at `PhoneWindow.<init>`.
//!
//! `ReflectFieldsMain` reads and writes every field type (static and instance,
//! mixed wide/narrow order) through both the typed and boxed `Field` API and
//! pins ART's `getDeclaredFields` order (instance then static).

mod common;

use common::{compile_case, run_ok};

#[test]
fn reflect_field_slots_match_art() {
    let Some(dex) = acos_test_support::require_some(compile_case(&["ReflectFieldsMain"]), "reflect_field_slots_match_art", "javac/d8/android.jar unavailable") else {
        return;
    };
    let Some(stdout) = run_ok(&dex, "ReflectFieldsMain", "ReflectFieldsMain") else {
        return;
    };
    assert_eq!(stdout, "reflect-fields-ok\n");
}
