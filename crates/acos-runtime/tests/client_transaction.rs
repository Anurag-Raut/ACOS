//! T-23 delivery test: build a real `ClientTransaction` and hand it to the real
//! `ActivityThread$ApplicationThread.scheduleTransaction`.
//!
//! `bindApplication` currently stops in the display/resource path
//! (`DisplayManagerGlobal.getOverlaySupport` reply parsing) before
//! `Application.onCreate`, which is outside this task's files. This test
//! therefore exercises the exact transaction machinery on a booted
//! `ActivityThread` **without** the bind step: the `LaunchActivityItem` and
//! `ResumeActivityItem` are real framework objects, `scheduleTransaction` is
//! the real receiver, and the main looper runs them until the next blocker.
//! It prints how far the launch got and the exact blocker instead of claiming
//! `Activity.onCreate` ran.

mod common;

use acos_apk::Apk;
use acos_runtime::{apk_loader, bind_app, client_transaction};
use acos_services::pms::{AppInfo, FIXTURE_UID};
use acos_vm::heap::NULL;
use common::{new_runtime, repo_root};

fn counter_apk() -> Option<std::path::PathBuf> {
    [
        repo_root().join("fixtures/build/apps/counter.apk"),
        repo_root().join("fixtures/build/apps/counter/base.apk"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

fn describe(vm: &mut acos_vm::Vm, h: acos_vm::heap::Handle) -> String {
    let mut out = String::new();
    let mut cur = h;
    for depth in 0..6 {
        if cur == NULL {
            break;
        }
        let class = vm
            .class_of_object(cur)
            .map(|c| vm.class_desc_of_id(c))
            .unwrap_or_else(|| "java.lang.Throwable".to_string());
        out.push_str(&format!("\n  [cause {depth}] "));
        match vm.throwable_message(cur) {
            Some(m) => out.push_str(&format!("{class}: {m}")),
            None => out.push_str(&class),
        }
        if let Some(entries) = vm.throwable_stacks.get(&cur) {
            for e in entries.iter().take(8) {
                out.push_str(&format!("\n      at {}.{}({})", e.class, e.method, e.line));
            }
        }
        cur = match vm.call_method(cur, "getCause", "()Ljava/lang/Throwable;", &[]) {
            Ok(acos_vm::javabridge::JavaValue::Object(next)) => next,
            _ => NULL,
        };
    }
    out
}

#[test]
fn client_transaction_is_built_and_delivered_to_the_real_application_thread() {
    let Some(apk) = counter_apk() else {
        eprintln!("SKIP client_transaction: fixtures/build/apps/counter.apk is absent");
        return;
    };
    let Some(mut rt) = new_runtime() else {
        return;
    };
    let manifest = Apk::open(&apk).expect("open counter apk").manifest().expect("manifest");
    let mut app = AppInfo::from_manifest(&manifest, &apk, FIXTURE_UID);
    apk_loader::stage(rt.vm(), &apk, &mut app).expect("stage counter apk");

    let boot = rt.boot_app_process(&apk).expect("boot app process");
    // Enter the real main looper, exactly like the lifecycle driver does.
    rt.vm_mut()
        .push_static_call("android.os.Looper", "loop", "()V", &[])
        .expect("push Looper.loop");

    let thread = boot.activity_thread;
    let app_thread = bind_app::app_thread(rt.vm_mut(), thread).expect("mAppThread");
    let tx = client_transaction::deliver_launch(rt.vm_mut(), app_thread, &app, &apk)
        .expect("build and schedule the real ClientTransaction");
    assert_ne!(tx.transaction, NULL);
    assert_ne!(tx.token, NULL);

    let result = acos_vm::interp::run(rt.vm_mut());
    let pending = rt.vm().pending_exception;
    let blocker = if pending != NULL {
        let msg = describe(rt.vm_mut(), pending);
        rt.vm_mut().pending_exception = NULL;
        Some(msg)
    } else {
        None
    };
    // `scheduleTransaction` -> `H.EXECUTE_TRANSACTION` must have created the
    // activity record (performLaunchActivity registers it before onCreate).
    let record = rt.vm_mut().call_method(
        thread,
        "getActivityClient",
        "(Landroid/os/IBinder;)Landroid/app/ActivityThread$ActivityClientRecord;",
        &[acos_vm::javabridge::JavaValue::Object(tx.token)],
    );
    let has_record = matches!(
        record,
        Ok(acos_vm::javabridge::JavaValue::Object(h)) if h != NULL
    );
    eprintln!(
        "client_transaction: scheduled=true record={has_record} run={result:?} blocker={blocker:?}"
    );
    assert!(
        result.is_ok() || blocker.is_some(),
        "transaction run neither completed nor produced a structured blocker"
    );
    assert!(
        has_record,
        "scheduleTransaction did not reach performLaunchActivity (blocker: {blocker:?})"
    );
    // If the framework got past resources, the app's own callback ran.
    if let Some(msg) = blocker {
        assert!(
            msg.contains("bad array lengths")
                || msg.contains("DisplayManager")
                || msg.contains("ResourcesManager")
                || msg.contains("OverlayProperties")
                || msg.contains("getDisplayInfo"),
            "unexpected blocker before Activity.onCreate: {msg}"
        );
        eprintln!("exact post-transaction blocker: {msg}");
    }
}
