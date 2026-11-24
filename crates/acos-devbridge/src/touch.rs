use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

const ACTION_DOWN: i32 = 0;
const ACTION_UP: i32 = 1;
const ACTION_MOVE: i32 = 2;
const ACTION_POINTER_DOWN: i32 = 5;
const ACTION_POINTER_UP: i32 = 6;
const ACTION_POINTER_INDEX_SHIFT: i32 = 8;

/// One motion frame ready for `acos serve` -> `panel_touch`.
#[derive(Debug, Clone, PartialEq)]
pub struct TouchFrame {
    pub action: i32,
    pub down_time_ns: i64,
    pub event_time_ns: i64,
    pub pointers: Vec<(i32, f32, f32)>,
}

/// Stateful converter from `EmulatorController.TouchEvent` (Protocol-B style
/// pointer list with `pressure == 0` meaning release) into Android
/// `MotionEvent` frames (`ACTION_DOWN`, `ACTION_POINTER_DOWN`, `ACTION_MOVE`,
/// `ACTION_POINTER_UP`, `ACTION_UP`).
#[derive(Debug, Default)]
pub struct TouchTracker {
    active: BTreeMap<i32, (f32, f32)>,
    down_time_ns: Option<i64>,
}

impl TouchTracker {
    pub fn apply(
        &mut self,
        touches: &[(i32, i32, i32, i32)], // (identifier, x, y, pressure)
        display_w: u32,
        display_h: u32,
    ) -> Vec<TouchFrame> {
        if touches.is_empty() {
            return Vec::new();
        }
        let max_x = display_w.saturating_sub(1) as f32;
        let max_y = display_h.saturating_sub(1) as f32;
        let clamp_xy = |x: i32, y: i32| -> (f32, f32) {
            (
                (x as f32).clamp(0.0, max_x),
                (y as f32).clamp(0.0, max_y),
            )
        };

        let now_ns = monotonic_nanos();
        let mut frames = Vec::new();
        let mut pending_move = false;
        let mut pending_move_stationary = false;

        let flush_move = |frames: &mut Vec<TouchFrame>,
                          active: &BTreeMap<i32, (f32, f32)>,
                          down_time_ns: Option<i64>,
                          pending_move: &mut bool,
                          pending_move_stationary: &mut bool,
                          allow_stationary: bool| {
            let should_emit =
                *pending_move || (allow_stationary && *pending_move_stationary && frames.is_empty());
            if should_emit && !active.is_empty() {
                let down_time_ns = down_time_ns.unwrap_or(now_ns);
                let pointers: Vec<(i32, f32, f32)> =
                    active.iter().map(|(&pid, &(px, py))| (pid, px, py)).collect();
                frames.push(TouchFrame {
                    action: ACTION_MOVE,
                    down_time_ns,
                    event_time_ns: now_ns,
                    pointers,
                });
            }
            *pending_move = false;
            *pending_move_stationary = false;
        };

        for &(id, x, y, pressure) in touches {
            let coords = clamp_xy(x, y);
            if pressure > 0 {
                if let Some(entry) = self.active.get_mut(&id) {
                    if *entry != coords {
                        *entry = coords;
                        pending_move = true;
                    } else {
                        pending_move_stationary = true;
                    }
                } else {
                    flush_move(
                        &mut frames,
                        &self.active,
                        self.down_time_ns,
                        &mut pending_move,
                        &mut pending_move_stationary,
                        false,
                    );
                    self.active.insert(id, coords);
                    let down_time_ns = match self.down_time_ns {
                        Some(t) => t,
                        None => {
                            self.down_time_ns = Some(now_ns);
                            now_ns
                        }
                    };
                    let pointers: Vec<(i32, f32, f32)> = self
                        .active
                        .iter()
                        .map(|(&pid, &(px, py))| (pid, px, py))
                        .collect();
                    let action = if pointers.len() == 1 {
                        ACTION_DOWN
                    } else {
                        let idx = pointers
                            .iter()
                            .position(|&(pid, _, _)| pid == id)
                            .unwrap_or(0) as i32;
                        ACTION_POINTER_DOWN | (idx << ACTION_POINTER_INDEX_SHIFT)
                    };
                    frames.push(TouchFrame {
                        action,
                        down_time_ns,
                        event_time_ns: now_ns,
                        pointers,
                    });
                }
            } else if self.active.contains_key(&id) {
                flush_move(
                    &mut frames,
                    &self.active,
                    self.down_time_ns,
                    &mut pending_move,
                    &mut pending_move_stationary,
                    false,
                );
                if let Some(entry) = self.active.get_mut(&id) {
                    *entry = coords;
                }
                let down_time_ns = self.down_time_ns.unwrap_or(now_ns);
                let pointers: Vec<(i32, f32, f32)> = self
                    .active
                    .iter()
                    .map(|(&pid, &(px, py))| (pid, px, py))
                    .collect();
                let action = if pointers.len() == 1 {
                    ACTION_UP
                } else {
                    let idx = pointers
                        .iter()
                        .position(|&(pid, _, _)| pid == id)
                        .unwrap_or(0) as i32;
                    ACTION_POINTER_UP | (idx << ACTION_POINTER_INDEX_SHIFT)
                };
                frames.push(TouchFrame {
                    action,
                    down_time_ns,
                    event_time_ns: now_ns,
                    pointers,
                });
                self.active.remove(&id);
                if self.active.is_empty() {
                    self.down_time_ns = None;
                }
            }
        }

        flush_move(
            &mut frames,
            &self.active,
            self.down_time_ns,
            &mut pending_move,
            &mut pending_move_stationary,
            true,
        );

        frames
    }
}

fn monotonic_nanos() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    if rc == 0 {
        ts.tv_sec * 1_000_000_000 + ts.tv_nsec
    } else {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_and_multi_touch_transitions() {
        let mut tracker = TouchTracker::default();
        let down = tracker.apply(&[(0, 100, 200, 1)], 1080, 2400);
        assert_eq!(down.len(), 1);
        assert_eq!(down[0].action, ACTION_DOWN);
        assert_eq!(down[0].pointers, vec![(0, 100.0, 200.0)]);

        let second = tracker.apply(&[(1, 300, 400, 1)], 1080, 2400);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].action, ACTION_POINTER_DOWN | (1 << 8));
        assert_eq!(
            second[0].pointers,
            vec![(0, 100.0, 200.0), (1, 300.0, 400.0)]
        );

        let moved = tracker.apply(&[(0, 120, 220, 1)], 1080, 2400);
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].action, ACTION_MOVE);
        assert_eq!(
            moved[0].pointers,
            vec![(0, 120.0, 220.0), (1, 300.0, 400.0)]
        );

        let release_first = tracker.apply(&[(0, 120, 220, 0)], 1080, 2400);
        assert_eq!(release_first.len(), 1);
        assert_eq!(release_first[0].action, ACTION_POINTER_UP); // idx 0
        assert_eq!(
            release_first[0].pointers,
            vec![(0, 120.0, 220.0), (1, 300.0, 400.0)]
        );

        let release_second = tracker.apply(&[(1, 300, 400, 0)], 1080, 2400);
        assert_eq!(release_second.len(), 1);
        assert_eq!(release_second[0].action, ACTION_UP);
        assert_eq!(release_second[0].pointers, vec![(1, 300.0, 400.0)]);
    }

    #[test]
    fn clamps_coordinates_to_display_bounds() {
        let mut tracker = TouchTracker::default();
        let down = tracker.apply(&[(0, -50, 9999, 1)], 1080, 2400);
        assert_eq!(down[0].pointers, vec![(0, 0.0, 2399.0)]);
    }

    #[test]
    fn release_then_reuse_same_pointer_in_single_event_single_finger() {
        let mut tracker = TouchTracker::default();
        let initial = tracker.apply(&[(0, 100, 200, 1)], 1080, 2400);
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].action, ACTION_DOWN);

        // Single TouchEvent releases pointer 0 and presses pointer 0 again at a new position.
        let reused = tracker.apply(&[(0, 110, 210, 0), (0, 500, 600, 1)], 1080, 2400);
        assert_eq!(reused.len(), 2, "expected UP then DOWN, got {reused:?}");
        assert_eq!(reused[0].action, ACTION_UP);
        assert_eq!(reused[0].pointers, vec![(0, 110.0, 210.0)]);
        assert_eq!(reused[1].action, ACTION_DOWN);
        assert_eq!(reused[1].pointers, vec![(0, 500.0, 600.0)]);

        // Pointer 0 must remain active so a subsequent move updates it rather than emitting a new DOWN.
        let next = tracker.apply(&[(0, 520, 620, 1)], 1080, 2400);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].action, ACTION_MOVE);
        assert_eq!(next[0].pointers, vec![(0, 520.0, 620.0)]);
    }

    #[test]
    fn release_then_reuse_same_pointer_in_single_event_multi_touch() {
        let mut tracker = TouchTracker::default();
        let _ = tracker.apply(&[(0, 100, 200, 1), (1, 300, 400, 1)], 1080, 2400);

        let reused = tracker.apply(&[(0, 105, 205, 0), (0, 700, 800, 1)], 1080, 2400);
        assert_eq!(reused.len(), 2, "expected POINTER_UP then POINTER_DOWN, got {reused:?}");
        assert_eq!(reused[0].action, ACTION_POINTER_UP); // pointer 0 is index 0
        assert_eq!(
            reused[0].pointers,
            vec![(0, 105.0, 205.0), (1, 300.0, 400.0)]
        );
        assert_eq!(reused[1].action, ACTION_POINTER_DOWN); // pointer 0 is index 0 again
        assert_eq!(
            reused[1].pointers,
            vec![(0, 700.0, 800.0), (1, 300.0, 400.0)]
        );
    }

    #[test]
    fn stationary_move_emits_move_only_without_transitions() {
        let mut tracker = TouchTracker::default();
        let _ = tracker.apply(&[(0, 100, 200, 1)], 1080, 2400);

        // Stationary pointer alone emits ACTION_MOVE.
        let stationary = tracker.apply(&[(0, 100, 200, 1)], 1080, 2400);
        assert_eq!(stationary.len(), 1);
        assert_eq!(stationary[0].action, ACTION_MOVE);

        // Stationary active pointer paired with a new pointer down does not emit a redundant MOVE.
        let down2 = tracker.apply(&[(0, 100, 200, 1), (1, 300, 400, 1)], 1080, 2400);
        assert_eq!(down2.len(), 1);
        assert_eq!(down2[0].action, ACTION_POINTER_DOWN | (1 << 8));
    }
}
