//! Property test: an arbitrary sequence of window/input/vsync operations must
//! never panic and must keep the state machines' invariants.

use std::sync::Arc;

use acos_parcel::binder::BinderId;
use acos_wm::clock::VirtualClock;
use acos_wm::codec::LayoutParams;
use acos_wm::display::DisplaySpec;
use acos_wm::facade::SessionWm;
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    AddWindow { id: u64 },
    RemoveWindow { id: u64 },
    Relayout { id: u64 },
    Tap { x: i32, y: i32, cause: i64 },
    Swipe { x0: i32, y0: i32, x1: i32, y1: i32, steps: u8 },
    Key { code: i32 },
    AckFirst,
    AckLast,
    Advance { nanos: u32 },
    PumpVsync,
    PumpAnr,
    Capture,
    UpdateVisible { types: i32 },
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (1u64..4).prop_map(|id| Op::AddWindow { id }),
        (1u64..4).prop_map(|id| Op::RemoveWindow { id }),
        (1u64..4).prop_map(|id| Op::Relayout { id }),
        (0i32..1200, 0i32..2500, 0i64..100).prop_map(|(x, y, cause)| Op::Tap { x, y, cause }),
        (0i32..1200, 0i32..2500, 0i32..1200, 0i32..2500, 1u8..6)
            .prop_map(|(x0, y0, x1, y1, steps)| Op::Swipe { x0, y0, x1, y1, steps }),
        (0i32..300).prop_map(|code| Op::Key { code }),
        Just(Op::AckFirst),
        Just(Op::AckLast),
        (0u32..60_000_000).prop_map(|nanos| Op::Advance { nanos }),
        Just(Op::PumpVsync),
        Just(Op::PumpAnr),
        Just(Op::Capture),
        (0i32..0x1fffff).prop_map(|types| Op::UpdateVisible { types }),
    ]
}

fn run(ops: &[Op]) {
    let clock = Arc::new(VirtualClock::new());
    let mut s = SessionWm::with_clock(DisplaySpec::reference_1080x2400(), clock.clone()).unwrap();
    // A pool of windows the ops can address.
    for id in 1..4u64 {
        let _ = s.add_window(
            BinderId(id),
            LayoutParams::application("pkg", "Main"),
            0,
        );
    }

    for op in ops {
        match op {
            Op::AddWindow { id } => {
                let _ = s.add_window(BinderId(*id), LayoutParams::application("pkg", "Main"), 0);
            }
            Op::RemoveWindow { id } => {
                let _ = s.remove_window(*id);
            }
            Op::Relayout { id } => {
                let _ = s.relayout(*id, LayoutParams::application("pkg", "Main"), -1);
            }
            Op::Tap { x, y, cause } => {
                let _ = s.tap(*x as f32, *y as f32, *cause);
            }
            Op::Swipe {
                x0,
                y0,
                x1,
                y1,
                steps,
            } => {
                let _ = s.swipe(
                    (*x0 as f32, *y0 as f32),
                    (*x1 as f32, *y1 as f32),
                    50_000_000,
                    *steps as usize,
                    -1,
                );
            }
            Op::Key { code } => {
                let _ = s.key(*code, -1);
            }
            Op::AckFirst => {
                if let Some(seq) = s.input.pending().keys().next().copied() {
                    let _ = s.ack(seq);
                }
            }
            Op::AckLast => {
                if let Some(seq) = s.input.pending().keys().next_back().copied() {
                    let _ = s.ack(seq);
                }
            }
            Op::Advance { nanos } => clock.advance(i64::from(*nanos)),
            Op::PumpVsync => {
                let _ = s.pump_vsync();
            }
            Op::PumpAnr => {
                let _ = s.pump_anr();
            }
            Op::Capture => {
                let _ = s.capture();
            }
            Op::UpdateVisible { types } => {
                for id in 1..4u64 {
                    let _ = s.wm.update_requested_visible_types(id, *types);
                }
            }
        }
        // Invariant: the vsync generator is idle exactly when no request is
        // pending (it never keeps a dangling timer).
        assert_eq!(
            s.vsync.is_idle(),
            s.vsync.pending_count() == 0 && s.vsync.next_deadline().is_none()
        );
        // Invariant: ack history never exceeds the sequence space.
        assert!(s.input.acks().len() < 1_000_000);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn random_operation_sequences_never_panic(ops in proptest::collection::vec(op_strategy(), 1..120)) {
        run(&ops);
    }
}

#[test]
fn a_fixed_workload_runs_cleanly() {
    run(&[
        Op::AddWindow { id: 1 },
        Op::Relayout { id: 1 },
        Op::Tap {
            x: 540,
            y: 427,
            cause: 1,
        },
        Op::AckFirst,
        Op::PumpVsync,
        Op::RemoveWindow { id: 1 },
    ]);
}
