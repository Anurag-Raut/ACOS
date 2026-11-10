//! Replay coverage assertions over the two measured binder traces.

use acos_wm::replay::{method_shape, run_default_replays, wm_owned, ReplayStatus};

#[test]
fn replay_covers_every_wm_owned_row() {
    let report = run_default_replays().unwrap();
    assert!(report.outcomes.len() >= 100, "both traces replayed");
    let errors: Vec<String> = report
        .outcomes
        .iter()
        .filter_map(|o| match &o.status {
            ReplayStatus::Error(e) => Some(format!("{}::{}: {e}", o.row.interface, o.row.method)),
            _ => None,
        })
        .collect();
    assert!(errors.is_empty(), "WM-owned replay errors: {errors:#?}");
    // Every WM-owned, non-transport row must be `ok`.
    for o in &report.outcomes {
        if wm_owned(&o.row.interface) && o.row.kind != "onTransact" {
            assert!(
                matches!(o.status, ReplayStatus::Ok),
                "{}::{} => {:?}",
                o.row.interface,
                o.row.method,
                o.status
            );
        }
    }
}

#[test]
fn coverage_table_counts_are_sane() {
    let report = run_default_replays().unwrap();
    let ok = report.count(|s| matches!(s, ReplayStatus::Ok));
    let not_owned = report.count(|s| matches!(s, ReplayStatus::NotOwned));
    let transport = report.count(|s| matches!(s, ReplayStatus::Transport));
    assert!(ok >= 40, "ok rows: {ok}");
    assert!(not_owned >= 10, "not-owned rows: {not_owned}");
    assert!(transport >= 5, "transport rows: {transport}");
    assert_eq!(ok + not_owned + transport, report.outcomes.len());
}

#[test]
fn aidl_tables_cover_the_owned_methods() {
    // The two relayout/addToDisplayAsUser layouts are the exact ones the crate
    // decodes; their derived shapes must be present in the AIDL table.
    let relayout = method_shape("android.view.IWindowSession", "relayout").expect("relayout");
    assert!(relayout.contains("ClientWindowFrames"));
    assert!(relayout.contains("MergedConfiguration"));
    assert!(relayout.contains("SurfaceControl"));
    let add = method_shape("android.view.IWindowSession", "addToDisplayAsUser").expect("add");
    assert!(add.contains("InputChannel"));
    assert!(add.contains("InsetsState"));
}
