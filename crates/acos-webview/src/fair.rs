//! Fair frame production across views (WVFAIR).
//!
//! `HeadlessExperimental.beginFrame` is a browser-global command in the pinned
//! engine: two targets asking at once crashed it in the two-WebView ART run
//! (`results/handoff/WV8/REPORT.md` §5), so frame production stays
//! **serialised per engine**. What used to serialise it was one mutex
//! (`Browser::render_lock`) held for up to three 2 s `beginFrame` attempts:
//! one slow page therefore starved every other WebView — including the frames
//! the input path needs — and a guest pull queue piled up behind it
//! (measured in WV8: `Choreographer: Skipped 119 frames`, a 2.0 s tap ack and
//! a lost click).
//!
//! This gate keeps the serialisation and removes the starvation:
//!
//! * **one attempt in flight per engine**, and an attempt is bounded by
//!   [`GateConfig::attempt`] (the caller's remaining deadline wins when it is
//!   shorter), so the worst case another view waits for is *one* attempt;
//! * **per-view coalescing** — at most one outstanding pull per view, however
//!   many `render` requests the guest sends: a second request joins the frame
//!   already being produced instead of queueing behind it (bounded in-flight);
//! * **a fair queue** — waiting views are granted the engine in priority order,
//!   then FIFO by arrival; a retry re-enters at the back, so the transient
//!   retries interleave with other views instead of running back to back;
//! * **input first** — the frames a tap/touch/key needs are granted before
//!   guest pull frames, and they queue behind at most the attempt already
//!   running;
//! * **no unbounded blocking** — a caller whose deadline passes while queued
//!   fails with a structured error instead of hanging on a mutex.
//!
//! The gate is per [`crate::broker::Browser`], i.e. one per host, so it also
//! orders the views of different sessions that share the broker (ADR-0031
//! item 3: one Chromium per host, one context per session).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::view::FrameData;
use crate::wire::code;

/// What a queued frame pull is for. Lower wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FramePriority {
    /// A frame the input path needs to route a gesture (`input.tap`,
    /// `input.touch`, `input.key`, `input.setFocus`).
    Input = 0,
    /// A guest `render` pull.
    Pull = 1,
}

impl FramePriority {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Pull => "pull",
        }
    }
}

/// One attempt's outcome, produced by the caller's closure.
pub enum Attempt {
    /// A frame to hand back (the engine produced one, or the surface still
    /// holds the last one — both are real pixels, never invented).
    Frame(Arc<FrameData>),
    /// Transient: nothing to hand over yet. The gate re-queues the pull at the
    /// back of the queue (giving other views the engine first) while attempts
    /// remain and the caller's deadline has not passed.
    Retry(String),
}

/// What a finished slot hands its followers.
#[derive(Clone)]
enum SlotOutcome {
    Frame(Arc<FrameData>),
    Failed { code: String, message: String },
}

impl SlotOutcome {
    fn into_result(self) -> Result<Arc<FrameData>> {
        match self {
            Self::Frame(frame) => Ok(frame),
            Self::Failed { code, message } => Err(Error::proto(code, message)),
        }
    }
}

struct Slot {
    inner: Mutex<Option<SlotOutcome>>,
}

impl Slot {
    fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    fn outcome(&self) -> Option<SlotOutcome> {
        self.inner.lock().expect("frame slot").clone()
    }

    fn complete(&self, outcome: SlotOutcome) {
        *self.inner.lock().expect("frame slot") = Some(outcome);
    }
}

struct Queued {
    seq: u64,
    view: u64,
    prio: FramePriority,
}

struct State {
    /// Monotonic arrival counter: the fair queue's tie-break.
    seq: u64,
    /// The view an attempt is running for, when one is (the engine is global).
    running: Option<u64>,
    /// The `seq` of the queued puller handed the engine next (the baton).
    granted: Option<u64>,
    queue: Vec<Queued>,
    /// The outstanding pull per view (exactly one: the coalescing point).
    slots: HashMap<u64, Arc<Slot>>,
}

/// The gate's limits.
#[derive(Debug, Clone, Copy)]
pub struct GateConfig {
    /// How long one `beginFrame` attempt may hold the engine **while another
    /// view is waiting**: that is the latency bound a waiting view sees. A
    /// caller's remaining deadline wins when it is shorter.
    pub attempt: Duration,
    /// How long an attempt may take when this engine serves **one** view — the
    /// same one-attempt budget the broker had before (`min(call timeout, 2 s)`).
    /// With a single WebView there is nobody to starve, so a page whose first
    /// frame is genuinely slow keeps the time it always had; from the second
    /// view on, every attempt is cut at [`Self::attempt`].
    pub attempt_idle: Duration,
    /// Attempts per pull (the three the retry policy has always had).
    pub max_attempts: u32,
    /// Pause between attempts (the engine settles a commit during it).
    pub retry_pause: Duration,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            attempt: Duration::from_millis(750),
            attempt_idle: Duration::from_secs(2),
            max_attempts: 3,
            retry_pause: Duration::from_millis(25),
        }
    }
}

/// The fairness counters reported in `engineStatus.frames`.
#[derive(Default)]
struct Counters {
    pulls: AtomicU64,
    coalesced: AtomicU64,
    attempts: AtomicU64,
    retries: AtomicU64,
    timeouts: AtomicU64,
    input_grants: AtomicU64,
    contended_attempts: AtomicU64,
    max_wait_ms: AtomicU64,
    max_attempt_ms: AtomicU64,
}

/// The engine's fair frame gate (one per [`crate::broker::Browser`]).
pub struct FrameGate {
    cfg: GateConfig,
    state: Mutex<State>,
    cv: Condvar,
    counters: Counters,
    /// Live views this engine serves ([`crate::broker::Broker`] keeps it in
    /// step on `view.create`/`view.destroy`). One view: no fairness budget is
    /// needed. Two or more: every attempt is cut at [`GateConfig::attempt`],
    /// because the next view's pull can arrive at any moment.
    open_views: AtomicU64,
}

impl FrameGate {
    #[must_use]
    pub fn new(cfg: GateConfig) -> Self {
        Self {
            cfg,
            state: Mutex::new(State {
                seq: 0,
                running: None,
                granted: None,
                queue: Vec::new(),
                slots: HashMap::new(),
            }),
            cv: Condvar::new(),
            counters: Counters::default(),
            open_views: AtomicU64::new(0),
        }
    }

    /// The engine now serves `n` live views (called by the broker on
    /// `view.create` / `view.destroy`).
    pub fn set_open_views(&self, n: usize) {
        self.open_views.store(n as u64, Ordering::SeqCst);
        self.cv.notify_all();
    }

    #[must_use]
    pub fn open_views(&self) -> usize {
        self.open_views.load(Ordering::SeqCst) as usize
    }

    #[must_use]
    pub fn config(&self) -> GateConfig {
        self.cfg
    }

    /// The counters, for `engineStatus.frames` (honest instrumentation: a
    /// fairness claim in a report is a measurement, not a reading of the code).
    #[must_use]
    pub fn stats(&self) -> Value {
        json!({
            "pulls": self.counters.pulls.load(Ordering::Relaxed),
            "coalesced": self.counters.coalesced.load(Ordering::Relaxed),
            "attempts": self.counters.attempts.load(Ordering::Relaxed),
            "retries": self.counters.retries.load(Ordering::Relaxed),
            "timeouts": self.counters.timeouts.load(Ordering::Relaxed),
            "inputGrants": self.counters.input_grants.load(Ordering::Relaxed),
            "contendedAttempts": self.counters.contended_attempts.load(Ordering::Relaxed),
            "maxWaitMs": self.counters.max_wait_ms.load(Ordering::Relaxed),
            "maxAttemptMs": self.counters.max_attempt_ms.load(Ordering::Relaxed),
            "attemptBudgetMs": self.cfg.attempt.as_millis() as u64,
            "attemptBudgetIdleMs": self.cfg.attempt_idle.as_millis() as u64,
            "openViews": self.open_views(),
            "maxAttempts": self.cfg.max_attempts,
        })
    }

    /// Produce one frame for `view`, fairly.
    ///
    /// `produce(attempt_timeout, final_attempt)` performs exactly one
    /// `beginFrame` attempt; it runs with the engine to itself and no gate lock
    /// held. `timeout` bounds the whole call, queue wait included.
    pub fn pull<F>(
        &self,
        view: u64,
        prio: FramePriority,
        timeout: Duration,
        mut produce: F,
    ) -> Result<Arc<FrameData>>
    where
        F: FnMut(Duration, bool) -> Attempt,
    {
        let deadline = Instant::now() + timeout;
        let arrived = Instant::now();
        self.counters.pulls.fetch_add(1, Ordering::Relaxed);
        let mut st = self.state.lock().expect("frame gate");

        // A pull already outstanding for this view: join it instead of adding
        // a second engine attempt (bounded in-flight; a burst of guest
        // invalidations must not become a queue of frames).
        if let Some(slot) = st.slots.get(&view).cloned() {
            self.counters.coalesced.fetch_add(1, Ordering::Relaxed);
            loop {
                if let Some(outcome) = slot.outcome() {
                    self.record_wait(arrived);
                    return outcome.into_result();
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    self.counters.timeouts.fetch_add(1, Ordering::Relaxed);
                    return Err(Error::proto(
                        code::TIMEOUT,
                        format!(
                            "render: view {view}'s pull is still in flight and did not finish \
                             within {timeout:?}"
                        ),
                    ));
                }
                let (next, _) = self.cv.wait_timeout(st, left).expect("frame gate wait");
                st = next;
            }
        }

        let mut seq = st.seq;
        st.seq += 1;
        let slot = Arc::new(Slot::new());
        st.slots.insert(view, Arc::clone(&slot));
        st.queue.push(Queued { seq, view, prio });
        self.grant_next(&mut st);
        self.cv.notify_all();

        let mut attempts = 0u32;
        let mut last_retry;
        loop {
            // Wait for the engine (fair queue, baton hand-off).
            while st.granted != Some(seq) {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    // Give up our place: other views must not wait for a
                    // caller that is already gone.
                    st.queue.retain(|q| q.seq != seq);
                    let message = format!(
                        "render: view {view} waited {timeout:?} behind other frames (a slow \
                         page holds the engine for at most {:?} per attempt)",
                        self.cfg.attempt
                    );
                    self.finish(&mut st, view, &slot, SlotOutcome::Failed {
                        code: code::TIMEOUT.to_string(),
                        message,
                    });
                    self.counters.timeouts.fetch_add(1, Ordering::Relaxed);
                    self.record_wait(arrived);
                    return Err(Error::proto(code::TIMEOUT, format!(
                        "render: view {view} waited {timeout:?} behind other frames"
                    )));
                }
                let (next, _) = self.cv.wait_timeout(st, left).expect("frame gate wait");
                st = next;
            }
            st.granted = None;
            st.running = Some(view);
            // Contended: this engine serves another view (or one is queued), so
            // this attempt is bounded by the fairness budget — the next view's
            // pull can arrive at any moment, and its wait is the bound. A
            // single-view engine keeps the whole attempt budget it always had,
            // so a slow first frame still paints.
            //
            // **Input frames always get the full budget**: their attempt is the
            // gesture itself (under `--enable-begin-frame-control` a touch only
            // reaches the page when a compositor frame is produced for it), and
            // an attempt cut short drops the user's input — the one failure mode
            // this gate must never introduce. They are already granted first, so
            // the worst case for everyone else is one such attempt.
            let contended = prio != FramePriority::Input
                && (!st.queue.is_empty() || self.open_views() > 1);
            drop(st);

            let left = deadline.saturating_duration_since(Instant::now());
            let budget = if contended {
                self.cfg.attempt
            } else {
                self.cfg.attempt_idle
            };
            if contended {
                self.counters.contended_attempts.fetch_add(1, Ordering::Relaxed);
            }
            let attempt_timeout = budget.min(left.max(Duration::from_millis(1)));
            let t0 = Instant::now();
            let outcome = produce(attempt_timeout, attempts + 1 >= self.cfg.max_attempts);
            let attempt_ms = t0.elapsed().as_millis() as u64;
            self.counters.attempts.fetch_add(1, Ordering::Relaxed);
            self.counters
                .max_attempt_ms
                .fetch_max(attempt_ms, Ordering::Relaxed);
            attempts += 1;

            st = self.state.lock().expect("frame gate");
            st.running = None;
            match outcome {
                Attempt::Frame(frame) => {
                    self.finish(
                        &mut st,
                        view,
                        &slot,
                        SlotOutcome::Frame(Arc::clone(&frame)),
                    );
                    self.record_wait(arrived);
                    return Ok(frame);
                }
                Attempt::Retry(message) => {
                    last_retry = message;
                    if attempts >= self.cfg.max_attempts
                        || Instant::now() >= deadline
                        || left.is_zero()
                    {
                        let detail = if last_retry.is_empty() {
                            format!("view {view}: no compositor frame after {attempts} attempts")
                        } else {
                            format!("view {view}: {last_retry}")
                        };
                        self.finish(&mut st, view, &slot, SlotOutcome::Failed {
                            code: code::FRAME_FAILED.to_string(),
                            message: detail.clone(),
                        });
                        self.record_wait(arrived);
                        return Err(Error::proto(code::FRAME_FAILED, detail));
                    }
                    // Re-enter at the back of the queue: the retries of a slow
                    // page interleave with every other view's pull instead of
                    // running back to back. The engine is handed on *before* the
                    // retry pause, so the pause costs no other view anything.
                    self.counters.retries.fetch_add(1, Ordering::Relaxed);
                    self.grant_next(&mut st);
                    self.cv.notify_all();
                    drop(st);
                    std::thread::sleep(self.cfg.retry_pause);
                    st = self.state.lock().expect("frame gate");
                    seq = st.seq;
                    st.seq += 1;
                    st.queue.push(Queued { view, prio, seq });
                    self.grant_next(&mut st);
                    self.cv.notify_all();
                }
            }
        }
    }

    /// Publish `outcome` to the slot's followers and hand the engine on.
    fn finish(&self, st: &mut State, view: u64, slot: &Arc<Slot>, outcome: SlotOutcome) {
        slot.complete(outcome);
        st.slots.remove(&view);
        self.grant_next(st);
        self.cv.notify_all();
    }

    /// Hand the engine to the next waiting puller (priority, then FIFO).
    fn grant_next(&self, st: &mut State) {
        if st.running.is_some() || st.granted.is_some() {
            return;
        }
        let Some(pos) = st
            .queue
            .iter()
            .enumerate()
            .min_by_key(|(_, q)| (q.prio, q.seq))
            .map(|(i, _)| i)
        else {
            return;
        };
        let next = st.queue.remove(pos);
        st.granted = Some(next.seq);
        if next.prio == FramePriority::Input {
            self.counters.input_grants.fetch_add(1, Ordering::Relaxed);
        }
        self.cv.notify_all();
    }

    fn record_wait(&self, arrived: Instant) {
        let ms = arrived.elapsed().as_millis() as u64;
        self.counters.max_wait_ms.fetch_max(ms, Ordering::Relaxed);
    }
}
