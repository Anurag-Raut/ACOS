//! Runtime-level acceptance for typed-text injection (`text_java`).
//!
//! The text-input fixture's `Activity` cannot launch on this HEAD: themed
//! activities stop at `activity_on_create` on the `VectorDrawable` native
//! (W68-A). The fixture's `EditText` is built by `LayoutInflater` only after
//! that, so the Rust bridge is exercised against a real Java `InputConnection`
//! implementation instead — `TextInputMain.Field` implements the actual
//! framework interface, so `commit_text_to` runs the real virtual dispatch and
//! `CharSequence` marshalling the framework's `EditableInputConnection` would.
//!
//! The launch-side evolution (focused-view discovery + `onCreateInputConnection`)
//! is covered by the unit tests in `text_java` and by the CLI dogfood; the
//! `EditText` end-to-end test becomes possible once W68-A lands.

mod common;

use acos_dex::DexFile;
use acos_runtime::text_java::{self, TextError, TextPath};
use acos_vm::heap::NULL;
use acos_vm::javabridge::JavaValue;
use common::{compile_case, new_runtime};

fn static_string(rt: &mut acos_runtime::Runtime, class: &str, name: &str) -> Option<String> {
    let vm = rt.vm_mut();
    match vm.get_static_field(class, name) {
        Ok(JavaValue::Object(h)) if h != NULL => vm.string_to_rust(h),
        _ => None,
    }
}

fn static_int(rt: &mut acos_runtime::Runtime, class: &str, name: &str) -> Option<i32> {
    match rt.vm_mut().get_static_field(class, name) {
        Ok(JavaValue::Int(v)) => Some(v),
        _ => None,
    }
}

#[test]
fn commit_text_calls_the_real_input_connection() {
    let Some(dex) = compile_case(&["TextInputMain"]) else {
        acos_test_support::require_prereq(false, "commit_text_calls_the_real_input_connection", "commit_text_calls_the_real_input_connection: javac/d8/android.jar unavailable");
        return;
    };
    let mut rt = new_runtime().expect("real-corelib runtime");
    rt.vm_mut()
        .add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let outcome = rt.run("TextInputMain", &[]);
    assert!(
        outcome.vm_error.is_none() && outcome.exit_code == 0,
        "TextInputMain failed: vm_error={:?}\nstdout:\n{}",
        outcome.vm_error,
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("textinput-ready"),
        "harness did not initialise:\n{}",
        outcome.stdout
    );

    let connection = match rt.vm_mut().get_static_field("TextInputMain", "connection") {
        Ok(JavaValue::Object(h)) if h != NULL => h,
        other => panic!("TextInputMain.connection is not an object: {other:?}"),
    };

    let report = text_java::commit_text_to(rt.vm_mut(), connection, "hello").expect("commitText");
    assert_eq!(report.path, TextPath::InputConnection);
    assert_eq!(report.characters, 5);
    assert!(
        report.ack_seqs.is_empty(),
        "commitText writes no input frames"
    );
    assert_eq!(
        static_string(&mut rt, "TextInputMain", "committed").as_deref(),
        Some("hello"),
        "the bridge must commit the exact text"
    );
    assert_eq!(
        static_int(&mut rt, "TextInputMain", "cursor"),
        Some(1),
        "an IME commit places the cursor at the end of the committed text"
    );
    assert_eq!(static_int(&mut rt, "TextInputMain", "commits"), Some(1));
    assert_eq!(
        static_int(&mut rt, "TextInputMain", "composing"),
        Some(0),
        "commitText must not open a composing region"
    );
}

#[test]
fn commit_text_refuses_what_it_cannot_type() {
    // The key-event fallback runs before any VM work, so its refusals are
    // testable without a runtime.
    assert_eq!(
        text_java::text_to_key_codes("Hi"),
        Err(TextError::UnsupportedCharacter('H'))
    );
    assert_eq!(
        text_java::text_to_key_codes("ok!").map(|_| ()),
        Err(TextError::UnsupportedCharacter('!'))
    );
}
