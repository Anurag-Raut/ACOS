//! Fair frame production across views (WVFAIR, WVTAIL).
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
//! * **input first, and it preempts** — the frames a tap/touch/key needs are
//!   granted before guest pull frames, an input pull that joins an outstanding
//!   guest pull promotes it to input priority, and an attempt that is *not* for
//!   input is **abandoned at its next slice** as soon as an input pull is
//!   waiting ([`AttemptCtx::preempted`]). Before this, an input frame's wait was
//!   the whole fairness budget (750 ms) even though the engine was only ever
//!   going to be needed by the gesture: measured 333 ms of dead wait before a
//!   1 ms input frame in `results/handoff/WVTAIL/` (task: a tap's end-to-end
//!   click under 1 s);
//! * **no unbounded blocking** — a caller whose deadline passes while queued
//!   fails with a structured error instead of hanging on a mutex.
//!
//! An abandoned attempt is honest about what it left behind: the engine still
//! finishes the frame it was told to produce (DevTools has no cancel), so its
//! reply arrives for a request nobody waits on any more; the next `beginFrame`
//! for that view can answer `Another frame is pending`, which the view treats as
//! the transient it is ([`crate::view::View::begin_frame`]) instead of handing
//! back the previous frame as if it were this one.
//!
//! The gate is per [`crate::broker::Browser`], i.e. one per host, so it also
//! orders the views of different sessions that share the broker (ADR-0031
//! item 3: one Chromium per host, one context per session).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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

/// What one attempt may do: its budget, whether it is the pull's last one, and
/// whether a queued input frame has taken the engine from it.
pub struct AttemptCtx<'a> {
    /// What this pull is for: a frame the guest asked for, or one a gesture needs
    /// (the two differ in what they may reuse — see [`crate::view::View::begin_frame`]).
    pub prio: FramePriority,
    /// The attempt's budget: the caller must answer within it.
    pub timeout: Duration,
    /// This is the pull's last attempt, so a transient failure is final.
    pub final_attempt: bool,
    /// How long one wait may be before the attempt has to look at
    /// [`Self::preempted`] again. An attempt is expected to wait in slices of
    /// this length (a `beginFrame` reply is awaited, not a mutex taken).
    pub slice: Duration,
    preempted: &'a AtomicBool,
}

impl AttemptCtx<'_> {
    /// `true` once a queued **input** pull needs the engine: the attempt must
    /// return [`Attempt::Yield`] at its next slice instead of holding the engine
    /// for its whole budget. Only non-input attempts are ever marked.
    #[must_use]
    pub fn preempted(&self) -> bool {
        self.preempted.load(Ordering::SeqCst)
    }

    /// How long this attempt may still wait before its next look at
    /// [`Self::preempted`].
    #[must_use]
    pub fn wait_slice(&self, left: Duration) -> Duration {
        left.min(self.slice).max(Duration::from_millis(1))
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
    /// An input frame took the engine mid-attempt ([`AttemptCtx::preempted`]).
    /// The gate re-queues the pull at the back **without** spending one of its
    /// attempts: the frame was never finished, so the pull lost no budget of its
    /// own, and the engine's late reply for it is dropped by the CDP client.
    Yield(String),
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
    /// An **input** pull joined this frame: from here on it is an input frame,
    /// so a re-queue keeps input priority (the gesture must not lose its place
    /// to the guest pulls that were queued after it).
    promoted: AtomicBool,
}

impl Slot {
    fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            promoted: AtomicBool::new(false),
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

/// The attempt running now (the engine is global, so there is at most one).
struct Running {
    view: u64,
    prio: FramePriority,
    /// Set when a queued input pull needs the engine: the attempt abandons at
    /// its next slice.
    preempted: Arc<AtomicBool>,
}

struct State {
    /// Monotonic arrival counter: the fair queue's tie-break.
    seq: u64,
    /// The attempt running now, when one is.
    running: Option<Running>,
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
    /// How long a non-input attempt waits before it looks for a queued input
    /// again. It is the worst case an input frame is made to wait for the
    /// attempt that was already running (measured 20 ms: the wait a tap pays
    /// instead of the whole fairness budget).
    pub preempt_slice: Duration,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            attempt: Duration::from_millis(750),
            attempt_idle: Duration::from_secs(2),
            max_attempts: 3,
            retry_pause: Duration::from_millis(25),
            preempt_slice: Duration::from_millis(20),
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
    /// Attempts abandoned because a queued input frame needed the engine.
    preemptions: AtomicU64,
    max_wait_ms: AtomicU64,
    max_attempt_ms: AtomicU64,
    /// Worst wait of an **input** pull for the engine, queue time included.
    input_max_wait_ms: AtomicU64,
}

/// Keeps [`FrameGate::input_waiters`] honest on every exit path (return, `?`,
/// panic in `produce`): the count is what marks running attempts preemptible.
struct InputWait<'a> {
    gate: &'a FrameGate,
    on: bool,
}

impl Drop for InputWait<'_> {
    fn drop(&mut self) {
        if self.on {
            self.gate.input_waiters.fetch_sub(1, Ordering::SeqCst);
            self.gate.cv.notify_all();
        }
    }
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
    /// Input pulls waiting for the engine (queued, or joined an outstanding
    /// pull). While it is not zero a non-input attempt is preemptible.
    input_waiters: AtomicUsize,
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
            input_waiters: AtomicUsize::new(0),
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

    /// Input pulls waiting for the engine right now.
    #[must_use]
    pub fn input_waiters(&self) -> usize {
        self.input_waiters.load(Ordering::SeqCst)
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
            "preemptions": self.counters.preemptions.load(Ordering::Relaxed),
            "maxWaitMs": self.counters.max_wait_ms.load(Ordering::Relaxed),
            "inputMaxWaitMs": self.counters.input_max_wait_ms.load(Ordering::Relaxed),
            "maxAttemptMs": self.counters.max_attempt_ms.load(Ordering::Relaxed),
            "attemptBudgetMs": self.cfg.attempt.as_millis() as u64,
            "attemptBudgetIdleMs": self.cfg.attempt_idle.as_millis() as u64,
            "preemptSliceMs": self.cfg.preempt_slice.as_millis() as u64,
            "openViews": self.open_views(),
            "inputWaiters": self.input_waiters(),
            "maxAttempts": self.cfg.max_attempts,
        })
    }

    /// Mark the attempt in flight preemptible (it is not an input attempt).
    ///
    /// Caller holds the state lock: the flag is the only thing the attempt's
    /// caller reads, and it sits on an `AtomicBool` so no lock is shared with
    /// the code that is inside the engine.
    fn take_engine_from_running(&self, st: &State) {
        if let Some(running) = st.running.as_ref() {
            if running.prio != FramePriority::Input {
                running.preempted.store(true, Ordering::SeqCst);
                self.cv.notify_all();
            }
        }
    }

    /// Produce one frame for `view`, fairly.
    ///
    /// `produce` performs exactly one `beginFrame` attempt; it runs with the
    /// engine to itself and no gate lock held, and it waits for the engine's
    /// reply in slices so a queued input frame can take the engine
    /// ([`AttemptCtx::preempted`]). `timeout` bounds the whole call, queue wait
    /// included.
    pub fn pull<F>(
        &self,
        view: u64,
        prio: FramePriority,
        timeout: Duration,
        mut produce: F,
    ) -> Result<Arc<FrameData>>
    where
        F: FnMut(AttemptCtx<'_>) -> Attempt,
    {
        let deadline = Instant::now() + timeout;
        let arrived = Instant::now();
        self.counters.pulls.fetch_add(1, Ordering::Relaxed);
        // Announce an input pull *before* taking the gate: an attempt that is in
        // the engine right now is not input, so it will not be needed for the
        // whole of its budget.
        let is_input = prio == FramePriority::Input;
        if is_input {
            self.input_waiters.fetch_add(1, Ordering::SeqCst);
        }
        let _input_wait = InputWait {
            gate: self,
            on: is_input,
        };
        let mut st = self.state.lock().expect("frame gate");

        // A pull already outstanding for this view: join it instead of adding
        // a second engine attempt (bounded in-flight; a burst of guest
        // invalidations must not become a queue of frames).
        if let Some(slot) = st.slots.get(&view).cloned() {
            self.counters.coalesced.fetch_add(1, Ordering::Relaxed);
            if is_input {
                // The gesture needs a frame carrying the touch it just
                // dispatched, so this frame *is* an input frame now: it takes
                // the engine from a non-input attempt and keeps input priority
                // when it re-queues.
                slot.promoted.store(true, Ordering::SeqCst);
                if let Some(q) = st.queue.iter_mut().find(|q| q.view == view) {
                    q.prio = FramePriority::Input;
                }
                self.take_engine_from_running(&st);
                self.grant_next(&mut st);
                self.cv.notify_all();
            }
            loop {
                if let Some(outcome) = slot.outcome() {
                    self.record_wait(arrived, prio);
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
        if is_input {
            self.take_engine_from_running(&st);
        }
        self.grant_next(&mut st);
        self.cv.notify_all();

        let mut attempts = 0u32;
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
                    self.record_wait(arrived, prio);
                    return Err(Error::proto(code::TIMEOUT, format!(
                        "render: view {view} waited {timeout:?} behind other frames"
                    )));
                }
                let (next, _) = self.cv.wait_timeout(st, left).expect("frame gate wait");
                st = next;
            }
            if is_input {
                self.counters
                    .input_max_wait_ms
                    .fetch_max(arrived.elapsed().as_millis() as u64, Ordering::Relaxed);
            }
            st.granted = None;
            let preempted = Arc::new(AtomicBool::new(false));
            st.running = Some(Running {
                view,
                prio,
                preempted: Arc::clone(&preempted),
            });
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
            // this gate must never introduce. They are already granted first and
            // they preempt, so the worst case for everyone else is one such
            // attempt.
            let contended = !is_input && (!st.queue.is_empty() || self.open_views() > 1);
            drop(st);

            let left = deadline.saturating_duration_since(Instant::now());
            let budget = if contended {
                self.cfg.attempt
            } else {
                self.cfg.attempt_idle
            };
            if contended {
                self.counters
                    .contended_attempts
                    .fetch_add(1, Ordering::Relaxed);
            }
            let attempt_timeout = budget.min(left.max(Duration::from_millis(1)));
            let ctx = AttemptCtx {
                prio,
                timeout: attempt_timeout,
                final_attempt: attempts + 1 >= self.cfg.max_attempts,
                slice: self.cfg.preempt_slice,
                preempted: &preempted,
            };
            let t0 = Instant::now();
            let outcome = produce(ctx);
            let attempt_ms = t0.elapsed().as_millis() as u64;
            self.counters
                .max_attempt_ms
                .fetch_max(attempt_ms, Ordering::Relaxed);
            let yielded = matches!(outcome, Attempt::Yield(_));
            if !yielded {
                self.counters.attempts.fetch_add(1, Ordering::Relaxed);
                attempts += 1;
            }

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
                    self.record_wait(arrived, prio);
                    return Ok(frame);
                }
                Attempt::Yield(reason) => {
                    // An input frame took the engine mid-attempt. The pull keeps
                    // its slot (a joined input caller must still get a frame)
                    // and re-enters the queue at the back, spending no attempt:
                    // the engine's late reply for this frame is dropped by the
                    // CDP client, and the next attempt for the view answers
                    // `Another frame is pending` until the engine is free.
                    self.counters.preemptions.fetch_add(1, Ordering::Relaxed);
                    self.counters.retries.fetch_add(1, Ordering::Relaxed);
                    self.grant_next(&mut st);
                    self.cv.notify_all();
                    drop(st);
                    std::thread::sleep(self.cfg.retry_pause);
                    st = self.state.lock().expect("frame gate");
                    let prio = self.effective_prio(&slot, prio);
                    seq = st.seq;
                    st.seq += 1;
                    st.queue.push(Queued { view, prio, seq });
                    self.grant_next(&mut st);
                    self.cv.notify_all();
                    let _ = reason;
                }
                Attempt::Retry(message) => {
                    if attempts >= self.cfg.max_attempts
                        || Instant::now() >= deadline
                        || left.is_zero()
                    {
                        let detail = if message.is_empty() {
                            format!("view {view}: no compositor frame after {attempts} attempts")
                        } else {
                            format!("view {view}: {message}")
                        };
                        self.finish(&mut st, view, &slot, SlotOutcome::Failed {
                            code: code::FRAME_FAILED.to_string(),
                            message: detail.clone(),
                        });
                        self.record_wait(arrived, prio);
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
                    let prio = self.effective_prio(&slot, prio);
                    seq = st.seq;
                    st.seq += 1;
                    st.queue.push(Queued { view, prio, seq });
                    self.grant_next(&mut st);
                    self.cv.notify_all();
                }
            }
        }
    }

    /// The priority a re-queued pull keeps: input once an input caller joined it.
    fn effective_prio(&self, slot: &Arc<Slot>, prio: FramePriority) -> FramePriority {
        if slot.promoted.load(Ordering::SeqCst) {
            FramePriority::Input
        } else {
            prio
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

    fn record_wait(&self, arrived: Instant, prio: FramePriority) {
        let ms = arrived.elapsed().as_millis() as u64;
        self.counters.max_wait_ms.fetch_max(ms, Ordering::Relaxed);
        if prio == FramePriority::Input {
            self.counters
                .input_max_wait_ms
                .fetch_max(ms, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    //! The gate's scheduling rules, measured on the gate itself: no engine, no
    //! ART, a `produce` closure that behaves like one `beginFrame` attempt. Each
    //! test names the behaviour it pins; the assertion messages carry the
    //! pre-WVTAIL number that the change improved
    //! (`results/handoff/WVTAIL/evidence/base/`).

    use super::*;

    fn frame(marker: u32) -> Arc<FrameData> {
        Arc::new(FrameData {
            seq: u64::from(marker),
            width: marker,
            height: 1,
            density: 1.0,
            format: crate::wire::FMT_PNG,
            stride: 4,
            damage: (0, 0, 0, 0),
            has_damage: false,
            engine_ms: 0.0,
            bytes: vec![marker as u8; 4],
        })
    }

    fn gate() -> Arc<FrameGate> {
        let gate = Arc::new(FrameGate::new(GateConfig {
            attempt: Duration::from_millis(750),
            attempt_idle: Duration::from_millis(750),
            max_attempts: 3,
            retry_pause: Duration::from_millis(1),
            preempt_slice: Duration::from_millis(5),
        }));
        // Two views: from the second view on, attempts are cut at `attempt`.
        gate.set_open_views(2);
        gate
    }

    /// Counters, read through the same JSON `engineStatus.frames` reports.
    fn counter(gate: &FrameGate, name: &str) -> u64 {
        gate.stats()[name].as_u64().unwrap_or(0)
    }

    /// A `produce` closure that behaves like a real attempt: it waits for the
    /// engine to answer in slices and gives the engine up when a queued input
    /// frame asks for it ([`AttemptCtx::preempted`]).
    fn patient_produce(
        holds: Duration,
        marker: u32,
    ) -> impl FnMut(AttemptCtx<'_>) -> Attempt {
        move |ctx: AttemptCtx<'_>| {
            let mut left = holds;
            loop {
                let slice = ctx.wait_slice(left);
                std::thread::sleep(slice);
                left = left.saturating_sub(slice);
                if ctx.preempted() {
                    return Attempt::Yield("preempted by a queued input frame".to_string());
                }
                if left.is_zero() {
                    return Attempt::Frame(frame(marker));
                }
            }
        }
    }

    /// A queued input frame takes the engine from the attempt already running:
    /// the input is granted within one preempt slice, not after the whole
    /// fairness budget.
    ///
    /// Before WVTAIL the input waited for the running attempt's entire budget
    /// (measured 333 ms of dead wait before a 1 ms input frame in
    /// `evidence/base/run4`, up to 750 ms by construction).
    #[test]
    fn a_queued_input_frame_preempts_the_running_non_input_attempt() {
        let gate = gate();
        let pull = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.pull(1, FramePriority::Pull, Duration::from_secs(5), patient_produce(
                    Duration::from_millis(750),
                    1,
                ))
            })
        };
        // Let the pull actually own the engine.
        std::thread::sleep(Duration::from_millis(60));
        let t0 = Instant::now();
        let input = gate
            .pull(2, FramePriority::Input, Duration::from_secs(5), |_ctx| {
                Attempt::Frame(frame(2))
            })
            .expect("the input frame");
        let input_ms = t0.elapsed().as_millis();
        let pull = pull.join().expect("pull thread").expect("the pull frame");

        assert_eq!(input.width, 2);
        assert_eq!(pull.width, 1, "the preempted pull must still get its frame");
        assert!(
            input_ms <= 100,
            "the input frame waited {input_ms} ms for the engine; it must wait about one \
             preempt slice ({} ms), not the running attempt's whole budget (750 ms)",
            gate.config().preempt_slice.as_millis()
        );
        assert!(
            counter(&gate, "preemptions") >= 1,
            "no attempt was preempted: the input only got the engine after the pull finished"
        );
        assert_eq!(counter(&gate, "inputGrants"), 1, "exactly one input grant");
        assert_eq!(gate.state.lock().unwrap().slots.len(), 0, "slots left behind");
    }

    /// Preemption does **not** spend the preempted pull's attempt budget: a pull
    /// that was abandoned for input frames keeps its three attempts, so a slow
    /// page whose frames keep being preempted still produces one.
    #[test]
    fn a_preempted_attempt_does_not_spend_the_pulls_attempt_budget() {
        let gate = gate();
        let pull = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.pull(1, FramePriority::Pull, Duration::from_secs(5), patient_produce(
                    Duration::from_millis(200),
                    7,
                ))
            })
        };
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(10));
            gate.pull(2, FramePriority::Input, Duration::from_secs(5), |_ctx| {
                Attempt::Frame(frame(3))
            })
            .expect("an input frame");
        }
        let pull = pull.join().expect("pull thread").expect("the pull frame");
        assert_eq!(pull.width, 7);
        let attempts = counter(&gate, "attempts");
        let preemptions = counter(&gate, "preemptions");
        assert!(
            preemptions >= 1,
            "the pull was never preempted, so nothing about the budget was measured"
        );
        assert!(
            attempts <= 4 + 1,
            "{attempts} attempts were counted for 4 input frames and 1 pull frame plus \
             {preemptions} preemptions: a yield must not spend an attempt"
        );
        assert_eq!(counter(&gate, "timeouts"), 0, "the pull timed out");
        assert_eq!(gate.state.lock().unwrap().slots.len(), 0, "slots left behind");
    }

    /// An input pull that joins an outstanding guest pull **promotes** it: the
    /// frame the gesture needs is an input frame from then on, so it keeps input
    /// priority when it re-queues instead of losing its place to the guest pulls
    /// queued after it. Both callers get the same frame.
    #[test]
    fn an_input_join_promotes_the_outstanding_pull_to_input_priority() {
        let gate = gate();
        let pull = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.pull(1, FramePriority::Pull, Duration::from_secs(5), patient_produce(
                    Duration::from_millis(300),
                    5,
                ))
            })
        };
        std::thread::sleep(Duration::from_millis(30));
        // The same view at input priority: joins the pull in flight.
        let joined = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.pull(1, FramePriority::Input, Duration::from_secs(5), |_ctx| {
                    panic!("the joined input pull must not produce a second frame")
                })
            })
        };
        let first = pull.join().expect("pull thread");
        let second = joined.join().expect("joined thread");
        let first = first.expect("the outstanding pull");
        let second = second.expect("the joined input pull");
        assert_eq!(first.width, 5);
        assert_eq!(second.width, 5, "the joiner must get the same frame");
        assert_eq!(
            counter(&gate, "coalesced"),
            1,
            "the input pull did not join the outstanding one"
        );
        assert!(
            counter(&gate, "inputGrants") >= 1,
            "the promoted frame was never granted as an input frame"
        );
    }

    /// A pull that never produces a frame fails with the structured
    /// `FRAME_FAILED` after its attempts, and leaves no slot behind — the caller
    /// is never handed invented pixels.
    #[test]
    fn a_pull_that_never_gets_a_frame_fails_and_leaves_no_slot() {
        let gate = gate();
        let err = gate
            .pull(3, FramePriority::Pull, Duration::from_secs(5), |_ctx| {
                Attempt::Retry("no frame yet".to_string())
            })
            .expect_err("a pull that never produces a frame must fail");
        match &err {
            Error::Proto { code: c, .. } => assert_eq!(c, code::FRAME_FAILED),
            other => panic!("expected FRAME_FAILED, got {other:?}"),
        }
        assert_eq!(counter(&gate, "attempts"), 3, "three attempts per pull");
        assert_eq!(gate.state.lock().unwrap().slots.len(), 0, "slots left behind");
    }
}
