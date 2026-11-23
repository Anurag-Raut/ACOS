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
        let mut any_move = false;

        // 1. Handle new pointer downs (`pressure > 0`, not currently active).
        for &(id, x, y, pressure) in touches {
            if pressure > 0 && !self.active.contains_key(&id) {
                let coords = clamp_xy(x, y);
                self.active.insert(id, coords);
                let down_time_ns = match self.down_time_ns {
                    Some(t) => t,
                    None => {
                        self.down_time_ns = Some(now_ns);
                        now_ns
                    }
                };
                let pointers: Vec<(i32, f32, f32)> =
                    self.active.iter().map(|(&pid, &(px, py))| (pid, px, py)).collect();
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
        }

        // 2. Handle pointer moves (`pressure > 0`, already active before step 1 or updated).
        // Skip pointers just inserted in step 1 unless duplicate entries appeared.
        for &(id, x, y, pressure) in touches {
            if pressure > 0 {
                let coords = clamp_xy(x, y);
                if let Some(entry) = self.active.get_mut(&id) {
                    if *entry != coords {
                        *entry = coords;
                        any_move = true;
                    } else if frames.is_empty() {
                        // Even stationary move packets from a client emit ACTION_MOVE
                        // if this TouchEvent contained no down/up transitions.
                        any_move = true;
                    }
                }
            }
        }
        if any_move && !self.active.is_empty() {
            let down_time_ns = self.down_time_ns.unwrap_or(now_ns);
            let pointers: Vec<(i32, f32, f32)> =
                self.active.iter().map(|(&pid, &(px, py))| (pid, px, py)).collect();
            frames.push(TouchFrame {
                action: ACTION_MOVE,
                down_time_ns,
                event_time_ns: now_ns,
                pointers,
            });
        }

        // 3. Handle pointer releases (`pressure == 0`, currently active).
        for &(id, x, y, pressure) in touches {
            if pressure == 0 && self.active.contains_key(&id) {
                let coords = clamp_xy(x, y);
                if let Some(entry) = self.active.get_mut(&id) {
                    *entry = coords;
                }
                let down_time_ns = self.down_time_ns.unwrap_or(now_ns);
                let pointers: Vec<(i32, f32, f32)> =
                    self.active.iter().map(|(&pid, &(px, py))| (pid, px, py)).collect();
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
}
