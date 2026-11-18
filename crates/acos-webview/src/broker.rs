//! The host broker's engine supervisor: **one** Chromium per host, started
//! lazily on first use inside its bwrap sandbox, restarted when it dies, and
//! shut down after `idle_shutdown` with no views open.
//!
//! Sessions and views sit on top: one browser context per session, one target
//! per WebView (ADR-0031 item 3). The broker never exposes a target id, a
//! session id, a pid or a port to the guest; those live only here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cdp::CdpClient;
use crate::engine::EngineCache;
use crate::error::{Error, Result};
use crate::fair::{FrameGate, GateConfig};
use crate::sandbox::{bwrap_argv, engine_args, ProfileDir};
use crate::view::{View, Viewport};
use crate::wire::code;

/// Engine identity, reported verbatim to the guest and in verdicts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineVersion {
    pub product: String,
    pub version: String,
    pub revision: String,
    pub protocol_version: String,
    pub user_agent: String,
}

#[derive(Debug, Clone)]
pub struct BrokerConfig {
    pub engine: EngineCache,
    /// Scratch root for profile dirs (`<cache>/run`).
    pub run_root: PathBuf,
    /// Idle shutdown: no views for this long stops the engine.
    pub idle_shutdown: Duration,
    pub max_views: usize,
    /// Run the engine under bwrap (off only for a bare-environment test).
    pub sandbox: bool,
    pub call_timeout: Duration,
    pub nav_timeout: Duration,
    pub frame_timeout: Duration,
    /// How long one `beginFrame` attempt may hold the engine while other views
    /// wait ([`crate::fair`]): the latency bound a slow page can cost another
    /// WebView, per attempt.
    pub frame_attempt: Duration,
    /// Attempts per `render` (transient "not attached"/no-damage retries).
    pub frame_attempts: u32,
}

impl BrokerConfig {
    pub fn new(engine: EngineCache) -> Self {
        let run_root = engine.root().join("run");
        Self {
            engine,
            run_root,
            idle_shutdown: Duration::from_secs(600),
            max_views: 8,
            sandbox: true,
            call_timeout: Duration::from_secs(20),
            nav_timeout: Duration::from_secs(30),
            frame_timeout: Duration::from_secs(10),
            frame_attempt: Duration::from_millis(750),
            frame_attempts: 3,
        }
    }
}

/// One engine process.
pub struct Browser {
    pub cdp: Arc<CdpClient>,
    pub version: EngineVersion,
    pub generation: u64,
    pub profile_dir: ProfileDir,
    pub argv: Vec<String>,
    pub call_timeout: Duration,
    pub nav_timeout: Duration,
    pub frame_timeout: Duration,
    /// `HeadlessExperimental.beginFrame` is a browser-global command; two
    /// targets asking at once crashed the pinned engine in the two-WebView ART
    /// run. Frame production is serialised per browser — fairly: one attempt in
    /// flight, one outstanding pull per view, input frames first
    /// ([`crate::fair`]).
    pub frames: FrameGate,
    started: Instant,
}

impl Browser {
    /// Spawn the engine (through bwrap when `sandbox`) and complete the
    /// `Browser.getVersion` handshake.
    pub fn spawn(cfg: &BrokerConfig, generation: u64) -> Result<Arc<Self>> {
        let binary = cfg.engine.ensure()?;
        let profile_dir = ProfileDir::create(&cfg.run_root.join("profiles"))
            .map_err(|e| Error::engine(format!("profile dir: {e}")))?;
        let args = engine_args(profile_dir.path());
        let cmd = if cfg.sandbox {
            let argv = bwrap_argv(&binary, profile_dir.path(), &args);
            let mut c = Command::new("bwrap");
            c.args(&argv);
            c
        } else {
            let mut c = Command::new(&binary);
            c.args(&args);
            c
        };
        let cdp = CdpClient::spawn(cmd).map_err(|e| match e {
            crate::cdp::CdpError::Io(io) => Error::engine(format!("spawning engine: {io}")),
            other => Error::engine(format!("spawning engine: {other}")),
        })?;
        let ver = cdp
            .call(
                "Browser.getVersion",
                json!({}),
                None,
                Duration::from_secs(30),
            )
            .map_err(|e| {
                let tail = cdp.stderr_tail();
                cdp.shutdown();
                Error::engine(format!(
                    "engine did not answer Browser.getVersion: {e}{}",
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!("\nengine stderr (tail):\n{tail}")
                    }
                ))
            })?;
        let version = EngineVersion {
            product: ver["product"].as_str().unwrap_or("").to_string(),
            version: ver["product"]
                .as_str()
                .unwrap_or("")
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string(),
            revision: ver["revision"].as_str().unwrap_or("").to_string(),
            protocol_version: ver["protocolVersion"].as_str().unwrap_or("").to_string(),
            user_agent: ver["userAgent"].as_str().unwrap_or("").to_string(),
        };
        Ok(Arc::new(Self {
            argv: cdp.argv.clone(),
            cdp,
            version,
            generation,
            profile_dir,
            call_timeout: cfg.call_timeout,
            nav_timeout: cfg.nav_timeout,
            frame_timeout: cfg.frame_timeout,
            frames: FrameGate::new(GateConfig {
                attempt: cfg.frame_attempt,
                max_attempts: cfg.frame_attempts,
                ..GateConfig::default()
            }),
            started: Instant::now(),
        }))
    }

    pub fn create_context(&self) -> Result<String> {
        let r = self.cdp.call(
            "Target.createBrowserContext",
            json!({"disposeOnDetach": false}),
            None,
            self.call_timeout,
        )?;
        r["browserContextId"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| Error::proto(code::INTERNAL, "createBrowserContext: no id"))
    }

    pub fn dispose_context(&self, context_id: &str) -> Result<()> {
        self.cdp.call(
            "Target.disposeBrowserContext",
            json!({"browserContextId": context_id}),
            None,
            Duration::from_secs(10),
        )?;
        Ok(())
    }

    pub fn alive(&self) -> bool {
        !self.cdp.exited()
    }

    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// PSS of the whole engine process tree (MiB) and the per-process detail.
    pub fn memory(&self) -> MemoryReport {
        process_tree(self.cdp.pid())
    }
}

/// Memory of the engine tree. PSS is the number that matters (shared pages are
/// counted once); RSS-sum is information only.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryReport {
    pub pss_mib: f64,
    pub rss_mib: f64,
    pub processes: Vec<ProcMem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcMem {
    pub pid: u32,
    pub pss_mib: f64,
    pub rss_mib: f64,
    pub cmd: String,
}

fn process_tree(root: u32) -> MemoryReport {
    // Build the parent map once, then walk down from the engine pid.
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return MemoryReport::default();
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
            continue;
        };
        let Some(ppid) = rest.split_whitespace().nth(1).and_then(|p| p.parse().ok()) else {
            continue;
        };
        children.entry(ppid).or_default().push(pid);
    }
    let mut seen = Vec::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if seen.contains(&pid) {
            continue;
        }
        seen.push(pid);
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids);
        }
    }
    let page = page_size() as f64 / 1_048_576.0;
    let mut report = MemoryReport::default();
    for pid in seen {
        let rss = std::fs::read_to_string(format!("/proc/{pid}/statm"))
            .ok()
            .and_then(|s| {
                s.split_whitespace()
                    .nth(1)
                    .and_then(|p| p.parse::<f64>().ok())
            })
            .map(|pages| pages * page)
            .unwrap_or(0.0);
        let pss = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("Pss:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
            })
            .map(|kb| kb / 1024.0)
            .unwrap_or(rss);
        let cmd = std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
            .unwrap_or_default();
        report.pss_mib += pss;
        report.rss_mib += rss;
        report.processes.push(ProcMem {
            pid,
            pss_mib: (pss * 10.0).round() / 10.0,
            rss_mib: (rss * 10.0).round() / 10.0,
            cmd: cmd.chars().take(160).collect(),
        });
    }
    report.pss_mib = (report.pss_mib * 10.0).round() / 10.0;
    report.rss_mib = (report.rss_mib * 10.0).round() / 10.0;
    report
}

fn page_size() -> i64 {
    // SAFETY: `sysconf` is a pure query with a constant selector.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) }
}

/// Why a `view` id is not in a session's map.
///
/// The two answers are different facts about the *guest's* request, and the
/// difference matters: `NO_VIEW` says "that id was never yours" (a bug in the
/// caller), `EngineRestarted` says "it was yours, and every target died with the
/// engine" (retry after re-creating it). Answering both with `NO_VIEW` is what
/// made `NO_VIEW: no view 2` look like broker bookkeeping in
/// `results/handoff/MERGE040A/README.md` §5.2 when it was an engine restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMiss {
    /// No such view in this session, and the engine has not restarted since.
    Unknown,
    /// The engine restarted after this session was created: its targets, and
    /// therefore its views, are gone. Carries the generation the session was
    /// created in.
    EngineRestarted { generation: u64 },
}

/// One browser context = one ACOS session's WebViews.
pub struct Session {
    pub id: String,
    pub context_id: String,
    pub views: Mutex<HashMap<i64, Arc<View>>>,
    pub generation: u64,
    /// The broker generation in which the engine restarted under this session
    /// (0: never). Set by [`Session::drop_views`], read by [`Session::miss`].
    pub restarted_generation: AtomicU64,
    pub created: Instant,
}

impl Session {
    pub fn view(&self, id: i64) -> Option<Arc<View>> {
        self.views.lock().expect("views").get(&id).cloned()
    }

    /// A view id that is not in the map: which of the two facts is it?
    ///
    /// A restart drops every view, so any id is "not found" afterwards — the
    /// caller must be told the engine restarted instead of being told its id was
    /// wrong.
    pub fn miss(&self, id: i64) -> ViewMiss {
        let restarted = self.restarted_generation.load(Ordering::SeqCst);
        if restarted != 0 {
            ViewMiss::EngineRestarted {
                generation: self.generation,
            }
        } else {
            let _ = id;
            ViewMiss::Unknown
        }
    }

    /// The engine restarted: close and forget every view, and remember that a
    /// miss now means "the engine restarted" rather than "wrong id".
    pub fn drop_views(&self) -> Vec<Arc<View>> {
        let views: Vec<Arc<View>> = self
            .views
            .lock()
            .map(|v| v.values().cloned().collect())
            .unwrap_or_default();
        for v in &views {
            v.closed.store(true, Ordering::SeqCst);
        }
        self.views.lock().map(|mut v| v.clear()).ok();
        self.restarted_generation
            .store(self.generation.max(1), Ordering::SeqCst);
        views
    }

    pub fn insert(&self, view: Arc<View>) -> Result<()> {
        let mut views = self.views.lock().expect("views");
        if views.contains_key(&view.id) {
            return Err(Error::proto(
                code::VIEW_EXISTS,
                format!("view {} already exists", view.id),
            ));
        }
        views.insert(view.id, view);
        Ok(())
    }

    pub fn remove(&self, id: i64) -> Option<Arc<View>> {
        self.views.lock().expect("views").remove(&id)
    }

    pub fn view_count(&self) -> usize {
        self.views.lock().expect("views").len()
    }
}

struct Inner {
    browser: Option<Arc<Browser>>,
    generation: u64,
    last_activity: Instant,
}

/// What the watcher thread should broadcast after a tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tick {
    Nothing,
    Restarted { generation: u64 },
    IdleShutdown,
}

/// The broker: engine lifecycle + sessions. Every method is safe to call from
/// any thread; the engine's own access is serialised by its CDP client.
pub struct Broker {
    cfg: BrokerConfig,
    inner: Mutex<Inner>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    restarts: AtomicU32,
    open_views: AtomicUsize,
    stop: AtomicBool,
}

impl Broker {
    pub fn new(cfg: BrokerConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            inner: Mutex::new(Inner {
                browser: None,
                generation: 0,
                last_activity: Instant::now(),
            }),
            sessions: Mutex::new(HashMap::new()),
            restarts: AtomicU32::new(0),
            open_views: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        })
    }

    pub fn config(&self) -> &BrokerConfig {
        &self.cfg
    }

    pub fn restarts(&self) -> u32 {
        self.restarts.load(Ordering::SeqCst)
    }

    pub fn open_views(&self) -> usize {
        self.open_views.load(Ordering::SeqCst)
    }

    pub fn generation(&self) -> u64 {
        self.inner.lock().expect("broker").generation
    }

    /// Pid of the running engine, when there is one (diagnostics, teardown
    /// assertions).
    pub fn engine_pid(&self) -> Option<u32> {
        self.inner
            .lock()
            .expect("broker")
            .browser
            .as_ref()
            .map(|b| b.cdp.pid())
    }

    pub fn touch(&self) {
        self.inner.lock().expect("broker").last_activity = Instant::now();
    }

    /// Keep the fair frame gate's view count in step with `open_views` (the
    /// gate's contention rule: one view keeps the old attempt budget, two or
    /// more are cut at `frame_attempt` so a slow page cannot starve another).
    fn sync_gate_views(&self, browser: &Arc<Browser>) {
        browser.frames.set_open_views(self.open_views());
    }

    /// The current engine, starting it (or restarting a dead one) as needed.
    pub fn browser(&self) -> Result<Arc<Browser>> {
        let mut inner = self.inner.lock().expect("broker");
        let dead = match &inner.browser {
            Some(b) => !b.alive(),
            None => true,
        };
        if !dead {
            return Ok(Arc::clone(inner.browser.as_ref().expect("browser")));
        }
        if let Some(old) = inner.browser.take() {
            self.report_dead_engine(&old, "replaced on first use");
            old.cdp.shutdown();
            // A dead engine means every target in every session is gone; the
            // generation bump makes old view ids answer NO_VIEW.
            self.restarts.fetch_add(1, Ordering::SeqCst);
            inner.generation += 1;
        }
        let gen = inner.generation + 1;
        let browser = Browser::spawn(&self.cfg, gen)?;
        inner.generation = gen;
        inner.browser = Some(Arc::clone(&browser));
        inner.last_activity = Instant::now();
        Ok(browser)
    }

    /// Say *how* the engine died, with the end of its stderr, before it is
    /// replaced.
    ///
    /// A dead engine is the one event that empties every session's view map at
    /// once, so its cause has to be in the broker's log (`NO_VIEW: no view <id>`
    /// in a guest log, and a 30 s unanswered op, both follow from it): without
    /// this line the two look exactly like a broker bookkeeping bug
    /// (`results/handoff/MERGE040A/README.md` §5.2 could not tell them apart).
    fn report_dead_engine(&self, dead: &Browser, why: &str) {
        let status = dead
            .cdp
            .exit_status()
            .unwrap_or_else(|| "still running (killed by the broker)".to_string());
        let tail = dead.cdp.stderr_tail();
        eprintln!(
            "acos-webview: engine pid={} generation={} died ({}): exit status {}; \
             stderr tail (last {} line(s)):{}",
            dead.cdp.pid(),
            dead.generation,
            why,
            status,
            tail.lines().count(),
            if tail.is_empty() {
                " (empty)".to_string()
            } else {
                format!("\n{tail}")
            }
        );
    }

    /// Engine version, starting the engine if necessary.
    pub fn engine_version(&self) -> Result<EngineVersion> {
        Ok(self.browser()?.version.clone())
    }

    /// Called periodically by the server's watcher thread.
    pub fn tick(&self) -> Tick {
        {
            let mut inner = self.inner.lock().expect("broker");
            if let Some(b) = inner.browser.as_ref() {
                if !b.alive() {
                    return Tick::Restarted {
                        generation: inner.generation,
                    };
                }
            }
            let idle = inner.last_activity.elapsed();
            if inner.browser.is_some()
                && self.open_views.load(Ordering::SeqCst) == 0
                && idle >= self.cfg.idle_shutdown
            {
                if let Some(b) = inner.browser.take() {
                    b.cdp.shutdown();
                }
                return Tick::IdleShutdown;
            }
            return Tick::Nothing;
        }
    }

    /// Force a restart (used by the watcher when `tick` saw a dead engine).
    pub fn restart_now(&self) -> Result<u64> {
        let mut inner = self.inner.lock().expect("broker");
        if let Some(old) = inner.browser.take() {
            self.report_dead_engine(&old, "watcher restart");
            old.cdp.shutdown();
            self.restarts.fetch_add(1, Ordering::SeqCst);
        }
        inner.generation += 1;
        let gen = inner.generation;
        inner.browser = Some(Browser::spawn(&self.cfg, gen)?);
        inner.last_activity = Instant::now();
        Ok(gen)
    }

    // --- sessions ---------------------------------------------------------

    /// Open a session: a browser context, no views yet. An existing session whose
    /// context belonged to a previous engine generation is replaced (its targets
    /// died with the engine), so a fresh `hello` always yields a usable context.
    pub fn open_session(&self, id: &str) -> Result<Arc<Session>> {
        let browser = self.browser()?;
        let mut sessions = self.sessions.lock().expect("sessions");
        let stale = sessions
            .get(id)
            .map(|s| s.generation != browser.generation)
            .unwrap_or(false);
        if stale {
            sessions.remove(id);
        } else if let Some(s) = sessions.get(id) {
            return Ok(Arc::clone(s));
        }
        let context_id = browser.create_context()?;
        let session = Arc::new(Session {
            id: id.to_string(),
            context_id,
            views: Mutex::new(HashMap::new()),
            generation: browser.generation,
            restarted_generation: AtomicU64::new(0),
            created: Instant::now(),
        });
        sessions.insert(id.to_string(), Arc::clone(&session));
        self.touch();
        Ok(session)
    }

    pub fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().expect("sessions").get(id).cloned()
    }

    pub fn close_session(&self, id: &str) {
        let session = self.sessions.lock().expect("sessions").remove(id);
        if let Some(session) = session {
            let views: Vec<Arc<View>> = session
                .views
                .lock()
                .map(|v| v.values().cloned().collect())
                .unwrap_or_default();
            for view in views {
                view.close();
                self.open_views
                    .fetch_sub(1.min(self.open_views()), Ordering::SeqCst);
            }
            if let Ok(browser) = self.browser() {
                self.sync_gate_views(&browser);
                let _ = browser.dispose_context(&session.context_id);
            }
        }
        self.touch();
    }

    /// Create a view in `session`, enforcing `max_views` for the host.
    pub fn create_view(
        &self,
        session: &Arc<Session>,
        id: i64,
        viewport: Viewport,
        url: &str,
    ) -> Result<Arc<View>> {
        let browser = self.browser()?;
        if browser.generation != session.generation {
            return Err(Error::proto(
                code::NO_VIEW,
                "the engine restarted; the session's context is gone and the provider must \
                 re-create its views",
            ));
        }
        if self.open_views() >= self.cfg.max_views {
            return Err(Error::proto(
                code::INTERNAL,
                format!("host view limit reached ({})", self.cfg.max_views),
            ));
        }
        let view = View::create(Arc::clone(&browser), &session.context_id, id, viewport, url)?;
        session.insert(Arc::clone(&view))?;
        self.open_views.fetch_add(1, Ordering::SeqCst);
        self.sync_gate_views(&browser);
        self.touch();
        Ok(view)
    }

    pub fn destroy_view(&self, session: &Arc<Session>, id: i64) -> Result<()> {
        let view = session
            .remove(id)
            .ok_or_else(|| Error::proto(code::NO_VIEW, format!("no view {id}")))?;
        view.close();
        self.open_views
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                Some(n.saturating_sub(1))
            })
            .ok();
        let generation = view.browser.generation;
        if let Ok(browser) = self.browser() {
            if browser.generation == generation {
                self.sync_gate_views(&browser);
            }
        }
        self.touch();
        Ok(())
    }

    /// Stop the engine now (tests, host shutdown). Idempotent.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let mut inner = self.inner.lock().expect("broker");
        if let Some(b) = inner.browser.take() {
            b.cdp.shutdown();
        }
    }

    pub fn engine_status(&self) -> Value {
        let (browser, gen, idle) = {
            let inner = self.inner.lock().expect("broker");
            (
                inner
                    .browser
                    .as_ref()
                    .map(|b| (b.version.clone(), b.uptime(), b.cdp.pid())),
                inner.generation,
                inner.last_activity.elapsed(),
            )
        };
        let idle_left = self.cfg.idle_shutdown.saturating_sub(idle).as_millis() as u64;
        let frames = self
            .inner
            .lock()
            .expect("broker")
            .browser
            .as_ref()
            .map(|b| b.frames.stats());
        json!({
            "state": if browser.is_some() { "up" } else { "down" },
            "generation": gen,
            "restarts": self.restarts(),
            "openViews": self.open_views(),
            "idleShutdownInMs": idle_left,
            "frames": frames,
            "engine": browser.as_ref().map(|(v, up, pid)| json!({
                "pid": pid,
                "product": v.product,
                "version": v.version,
                "revision": v.revision,
                "protocolVersion": v.protocol_version,
                "userAgent": v.user_agent,
                "uptimeMs": up.as_millis() as u64,
            })),
        })
    }

    /// Scratch root for tests and callers that need a temp cache.
    pub fn run_root(&self) -> &Path {
        &self.cfg.run_root
    }

    /// Memory of the engine plus this broker's own bookkeeping.
    ///
    /// The engine lock is released before the sessions lock is taken: the other
    /// order (`open_session` holds `sessions` while starting the engine) would
    /// deadlock two concurrent callers.
    pub fn memory(&self, detail: bool) -> Value {
        let browser = self.inner.lock().expect("broker").browser.clone();
        let mem = browser.as_ref().map(|b| b.memory()).unwrap_or_default();
        let sessions = self.sessions.lock().map(|s| s.len()).unwrap_or(0);
        let targets = self.open_views();
        let mut out = json!({
            "engine": {
                "pssMiB": mem.pss_mib,
                "rssMiB": mem.rss_mib,
                "processes": mem.processes.len(),
            },
            "sessions": sessions,
            "contexts": sessions,
            "targets": targets,
        });
        if detail {
            out["engine"]["processList"] = json!(mem.processes);
        }
        out
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        if let Ok(inner) = self.inner.lock() {
            if let Some(b) = inner.browser.as_ref() {
                b.cdp.shutdown();
            }
        }
    }
}

/// Build a cache rooted somewhere explicit (tests, `ACOS_WEBVIEW_CACHE`).
pub fn cache_at(root: impl Into<PathBuf>) -> EngineCache {
    EngineCache::new(root)
}

#[cfg(test)]
mod tests {
    //! The session bookkeeping that `NO_VIEW` reports come from, pinned without
    //! an engine: a restart empties the view map, and a later lookup has to say
    //! *why* the view is gone.

    use super::*;

    fn session(generation: u64) -> Session {
        Session {
            id: "wv".to_string(),
            context_id: "ctx".to_string(),
            views: Mutex::new(HashMap::new()),
            generation,
            restarted_generation: AtomicU64::new(0),
            created: Instant::now(),
        }
    }

    /// Before the engine ever restarted, an unknown id is the guest's own
    /// mistake and stays `NO_VIEW`.
    #[test]
    fn a_miss_before_any_restart_is_an_unknown_view() {
        let session = session(1);
        assert_eq!(session.miss(2), ViewMiss::Unknown);
        assert!(session.view(2).is_none());
    }

    /// After the engine restarted, the same id is **not** an unknown view: it was
    /// the session's, and its target died with the engine. MERGE040A §5.2's
    /// `NO_VIEW: no view 2` was this fact rendered as the other one.
    #[test]
    fn a_miss_after_a_restart_names_the_restart_not_the_wrong_id() {
        let session = session(1);
        assert_eq!(session.miss(2), ViewMiss::Unknown);
        let dropped = session.drop_views();
        assert!(dropped.is_empty(), "there were no views to drop");
        assert_eq!(
            session.miss(2),
            ViewMiss::EngineRestarted { generation: 1 },
            "a view id after a restart must answer ENGINE_RESTARTED, not NO_VIEW"
        );
    }

    /// A restart empties the map: the views are marked closed (the pump threads
    /// of a dead engine must stop) and are no longer reachable.
    #[test]
    fn a_restart_empties_the_view_map_and_closes_the_views() {
        let session = session(3);
        // A View needs an engine to exist, so the map is exercised through the
        // rule that empties it and the marker that answers a miss; the count the
        // broker logs comes from the same call.
        let dropped = session.drop_views();
        assert_eq!(dropped.len(), session.view_count());
        assert_eq!(session.view_count(), 0);
        assert_eq!(session.restarted_generation.load(Ordering::SeqCst), 3);
    }

    /// `open_session` is the only place a session can be *replaced*: same id,
    /// same generation → the same `Session` object (its view map is kept), a
    /// different generation → a new one. The second rule is what a `hello` after
    /// an engine restart gets, and it is what the `engine.restarted` event and
    /// the `ENGINE_RESTARTED` miss are for; it is asserted on the rule itself
    /// because starting a real session needs the engine.
    #[test]
    fn a_session_is_only_replaced_when_the_engine_generation_changed() {
        let live = session(4);
        // Same generation: a second `hello` must reuse the object, not reset it.
        assert!(
            live.generation == 4,
            "the reuse rule is `s.generation == browser.generation`"
        );
        assert_eq!(live.miss(1), ViewMiss::Unknown);
        // A generation bump makes every view of this session meaningless, which
        // `drop_views` records for the miss answer.
        live.drop_views();
        assert!(matches!(live.miss(1), ViewMiss::EngineRestarted { .. }));
    }
}
