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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cdp::CdpClient;
use crate::engine::EngineCache;
use crate::error::{Error, Result};
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
    /// targets asking at once crashed the pinned engine in the two-WebView
    /// ART run. Frame production is serialised per browser (measured).
    pub render_lock: Mutex<()>,
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
            render_lock: Mutex::new(()),
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

/// One browser context = one ACOS session's WebViews.
pub struct Session {
    pub id: String,
    pub context_id: String,
    pub views: Mutex<HashMap<i64, Arc<View>>>,
    pub generation: u64,
    pub created: Instant,
}

impl Session {
    pub fn view(&self, id: i64) -> Option<Arc<View>> {
        self.views.lock().expect("views").get(&id).cloned()
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
                    .map(|b| (b.version.clone(), b.uptime())),
                inner.generation,
                inner.last_activity.elapsed(),
            )
        };
        let idle_left = self.cfg.idle_shutdown.saturating_sub(idle).as_millis() as u64;
        json!({
            "state": if browser.is_some() { "up" } else { "down" },
            "generation": gen,
            "restarts": self.restarts(),
            "openViews": self.open_views(),
            "idleShutdownInMs": idle_left,
            "engine": browser.as_ref().map(|(v, up)| json!({
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
