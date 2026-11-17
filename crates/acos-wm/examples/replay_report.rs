//! `replay_report` — run the measured binder traces through the WM handlers and
//! write the coverage table to `results/wm/replay_coverage.txt`.
//!
//! ```text
//! cargo run -p acos-wm --example replay_report
//! ```

use acos_wm::replay::{run_default_replays, workspace_root, ReplayStatus};

fn main() {
    let report = run_default_replays().expect("replay");
    let table = report.coverage_table();
    let path = workspace_root().join("results/wm/replay_coverage.txt");
    std::fs::create_dir_all(path.parent().unwrap()).expect("results/wm");
    std::fs::write(&path, &table).expect("write replay_coverage.txt");

    let ok = report.count(|s| matches!(s, ReplayStatus::Ok));
    let err = report.count(|s| matches!(s, ReplayStatus::Error(_)));
    let not_owned = report.count(|s| matches!(s, ReplayStatus::NotOwned));
    let transport = report.count(|s| matches!(s, ReplayStatus::Transport));
    println!("wrote {}", path.display());
    println!(
        "rows={} ok={ok} error={err} not-owned={not_owned} transport={transport}",
        report.outcomes.len()
    );

    if err > 0 {
        for o in &report.outcomes {
            if let ReplayStatus::Error(e) = &o.status {
                eprintln!("ERROR {}::{}: {e}", o.row.interface, o.row.method);
            }
        }
        std::process::exit(1);
    }
}
