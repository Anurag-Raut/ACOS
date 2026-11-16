//! One View = one DevTools target in the session's browser context.
//!
//! Everything the guest provider needs from a page lives here: viewport and
//! device metrics, navigation + the callbacks derived from DevTools events,
//! JavaScript both ways, deterministic frames, input, request-interception
//! pauses and the accessibility tree exported in **display coordinates**.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::ax::{self, AxNode};
use crate::broker::Browser;
use crate::error::{Error, Result};
use crate::wire::{code, FMT_PNG};

/// The page's size in device pixels and the Android density it maps to.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub density: f64,
}

impl Viewport {
    pub fn new(width: u32, height: u32, density: f64) -> Result<Self> {
        if width == 0 || height == 0 || width > 20_000 || height > 20_000 {
            return Err(Error::proto(
                code::INVALID_VIEWPORT,
                format!("viewport {width}x{height} is out of range"),
            ));
        }
        if !(density.is_finite() && density > 0.0 && density <= 16.0) {
            return Err(Error::proto(
                code::INVALID_VIEWPORT,
                format!("density {density} is out of range"),
            ));
        }
        Ok(Self {
            width,
            height,
            density,
        })
    }
}

/// Navigation bookkeeping shared with the pump thread. `load_url` waits on it,
/// the pump advances it; there is exactly one event consumer per view.
#[derive(Default)]
pub struct NavState {
    inner: Mutex<NavInner>,
    cv: Condvar,
}

#[derive(Default)]
struct NavInner {
    started: u64,
    finished: u64,
    last_url: String,
    last_title: String,
    can_go_back: bool,
    can_go_forward: bool,
    failure: Option<(String, String)>,
    load_error: Option<(String, String)>,
}

impl NavState {
    fn bump_started(&self, url: &str) {
        let mut g = self.inner.lock().expect("nav");
        g.started += 1;
        g.last_url = url.to_string();
        g.failure = None;
        self.cv.notify_all();
    }

    fn bump_finished(&self, url: &str, title: &str, back: bool, forward: bool) {
        let mut g = self.inner.lock().expect("nav");
        g.finished += 1;
        g.last_url = url.to_string();
        g.last_title = title.to_string();
        g.can_go_back = back;
        g.can_go_forward = forward;
        self.cv.notify_all();
    }

    fn fail(&self, code: &str, message: &str) {
        let mut g = self.inner.lock().expect("nav");
        g.failure = Some((code.to_string(), message.to_string()));
        self.cv.notify_all();
    }

    /// Wait until a load that started after `after_started` finishes (or fails).
    fn wait_load(&self, after_started: u64, timeout: Duration) -> Result<(String, String)> {
        let deadline = Instant::now() + timeout;
        let mut g = self.inner.lock().expect("nav");
        loop {
            if let Some((code, message)) = &g.failure {
                return Err(Error::proto(
                    code::NAVIGATION_FAILED,
                    format!("{code}: {message}"),
                ));
            }
            if g.finished > after_started {
                return Ok((g.last_url.clone(), g.last_title.clone()));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::proto(
                    code::TIMEOUT,
                    format!(
                        "navigation to a page that started at {after_started} never finished"
                    ),
                ));
            }
            let (next, _) = self.cv.wait_timeout(g, left).expect("nav wait");
            g = next;
        }
    }

    pub fn snapshot(&self) -> Value {
        let g = self.inner.lock().expect("nav");
        json!({
            "url": g.last_url,
            "title": g.last_title,
            "canGoBack": g.can_go_back,
            "canGoForward": g.can_go_forward,
            "started": g.started,
            "finished": g.finished,
            "error": g.load_error.as_ref().map(|(c, m)| json!({"code": c, "message": m})),
        })
    }
}

/// One derived event, produced by [`View::pump_once`] and sent to the guest by
/// the connection that owns the view.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewEvent {
    NavigationStarted {
        url: String,
        is_new: bool,
        is_redirect: bool,
    },
    NavigationRequested {
        url: String,
        is_redirect: bool,
    },
    NavigationCommitted {
        url: String,
    },
    NavigationFinished {
        url: String,
        title: String,
        can_go_back: bool,
        can_go_forward: bool,
    },
    NavigationFailed {
        url: String,
        code: String,
        message: String,
    },
    RenderInvalidated {
        reason: String,
    },
    RequestPaused {
        request_id: String,
        url: String,
        method: String,
        resource_type: String,
        headers: Value,
        is_asset: bool,
    },
    BindingCalled {
        name: String,
        payload: String,
    },
}

/// A rendered frame (`render`).
#[derive(Debug, Clone)]
pub struct FrameData {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub density: f64,
    pub format: u8,
    /// Byte length of the encoded frame (`png`: the PNG's size).
    pub stride: u32,
    pub bytes: Vec<u8>,
    /// Device-pixel damage rectangle; the whole viewport when Chromium reports
    /// damage without a rect (`beginFrame` reports a boolean, not a region).
    pub damage: (u32, u32, u32, u32),
    pub has_damage: bool,
    pub engine_ms: f64,
}

/// The JavaScript bridge shim: one binding, one pending table, one installer.
const JSI_SHIM: &str = r#"
(() => {
  if (window.__acos_jsi_installed) return 'already';
  window.__acos_jsi_installed = true;
  window.__acos_jsi_pending = new Map();
  window.__acos_jsi_seq = 0;
  window.__acos_jsi_resolve = (callId, resultJson, error) => {
    const p = window.__acos_jsi_pending.get(String(callId));
    if (!p) return false;
    window.__acos_jsi_pending.delete(String(callId));
    if (error) { p.reject(new Error(String(error))); }
    else { try { p.resolve(JSON.parse(resultJson)); } catch (e) { p.resolve(resultJson); } }
    return true;
  };
  window.__acos_jsi_install = (name, methods) => {
    const iface = {};
    for (const m of methods) {
      iface[m] = (...args) => new Promise((resolve, reject) => {
        const callId = String(++window.__acos_jsi_seq);
        window.__acos_jsi_pending.set(callId, { resolve, reject });
        window.__acos_jsi(JSON.stringify({ name, method: m, args, callId }));
      });
    }
    window[name] = iface;
    return methods.length;
  };
  return 'installed';
})()
"#;

/// One target = one WebView instance.
pub struct View {
    pub id: i64,
    pub browser: Arc<Browser>,
    pub context_id: String,
    pub target_id: String,
    pub cdp_session: String,
    pub events: Arc<crate::cdp::EventQueue>,
    pub viewport: Mutex<Viewport>,
    pub nav: Arc<NavState>,
    pub seq: AtomicU64,
    pub bindings: Mutex<Vec<String>>,
    /// Registered JS interfaces, re-installed on every navigation: the page
    /// shim lives in the document, and a new document (a `view.loadUrl` after
    /// `addJavascriptInterface`) drops it. Android's interfaces survive
    /// navigation; this keeps that contract (measured: the page's button ran
    /// its JS but `window.acosBridge.report(...)` was gone after the load).
    pub interfaces: Mutex<HashMap<String, Vec<String>>>,
    pub main_frame: Mutex<Option<String>>,
    pub closed: AtomicBool,
    pub created: Instant,
}

impl View {
    /// Create the target in `context_id`, attach a flattened CDP session and
    /// enable the domains the broker uses.
    pub fn create(
        browser: Arc<Browser>,
        context_id: &str,
        id: i64,
        viewport: Viewport,
        url: &str,
    ) -> Result<Arc<Self>> {
        let cdp = &browser.cdp;
        let target = cdp.call(
            "Target.createTarget",
            json!({"url": url, "browserContextId": context_id}),
            None,
            browser.call_timeout,
        )?;
        let target_id = target["targetId"]
            .as_str()
            .ok_or_else(|| Error::proto(code::INTERNAL, "Target.createTarget: no targetId"))?
            .to_string();
        let attached = cdp.call(
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
            None,
            browser.call_timeout,
        )?;
        let session = attached["sessionId"]
            .as_str()
            .ok_or_else(|| Error::proto(code::INTERNAL, "attachToTarget: no sessionId"))?
            .to_string();
        let events = cdp.queue(&session);
        let view = Arc::new(Self {
            id,
            browser: Arc::clone(&browser),
            context_id: context_id.to_string(),
            target_id,
            cdp_session: session,
            events,
            viewport: Mutex::new(viewport),
            nav: Arc::new(NavState::default()),
            seq: AtomicU64::new(0),
            bindings: Mutex::new(Vec::new()),
            interfaces: Mutex::new(HashMap::new()),
            main_frame: Mutex::new(None),
            closed: AtomicBool::new(false),
            created: Instant::now(),
        });
        for (method, params) in [
            ("Page.enable", json!({})),
            ("Runtime.enable", json!({})),
            ("DOM.enable", json!({})),
            ("Accessibility.enable", json!({})),
            ("Log.enable", json!({})),
        ] {
            // A page that refuses a domain (a crashed target) is a real error,
            // not something to paper over.
            view.call(method, params, browser.call_timeout)?;
        }
        // Pause asset/content schemes only: `file:///android_asset` and
        // `content://` are served by the guest over the wire (WIRE.md §4.3).
        // `http(s)` interception is opt-in per view (`set_intercept_all`).
        view.call(
            "Fetch.enable",
            json!({"patterns": [
                {"urlPattern": "file:///*", "requestStage": "Request"},
                {"urlPattern": "content://*", "requestStage": "Request"}
            ]}),
            browser.call_timeout,
        )?;
        view.set_viewport(viewport)?;
        Ok(view)
    }

    /// One CDP call in this view's session.
    pub fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        Ok(self
            .browser
            .cdp
            .call(method, params, Some(&self.cdp_session), timeout)?)
    }

    fn send(&self, method: &str, params: Value) -> Result<crate::cdp::PendingReply> {
        Ok(self
            .browser
            .cdp
            .send(method, params, Some(&self.cdp_session))?)
    }

    pub fn viewport(&self) -> Viewport {
        *self.viewport.lock().expect("viewport")
    }

    /// Opt in to pausing every request (`shouldInterceptRequest` fidelity).
    pub fn set_intercept_all(&self, enabled: bool) -> Result<()> {
        let patterns = if enabled {
            json!([{"urlPattern": "*", "requestStage": "Request"}])
        } else {
            json!([
                {"urlPattern": "file:///*", "requestStage": "Request"},
                {"urlPattern": "content://*", "requestStage": "Request"}
            ])
        };
        self.call(
            "Fetch.enable",
            json!({"patterns": patterns}),
            self.browser.call_timeout,
        )?;
        Ok(())
    }

    pub fn set_viewport(&self, vp: Viewport) -> Result<()> {
        *self.viewport.lock().expect("viewport") = vp;
        // WIRE.md §4: the wire carries **device** pixels; DevTools
        // `setDeviceMetricsOverride` takes CSS pixels and applies
        // `deviceScaleFactor`. Passing device pixels straight through made the
        // page 2.625x too large: a full-width heading's AX box was 2530 device
        // px in a 900 px View (measured), so page text landed outside the View
        // and a tap by its bounds missed. Divide here, once.
        let css_width = (vp.width as f64 / vp.density).round().max(1.0) as u32;
        let css_height = (vp.height as f64 / vp.density).round().max(1.0) as u32;
        self.call(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": css_width,
                "height": css_height,
                "deviceScaleFactor": vp.density,
                // `mobile: true` selects Chromium's wide (980 CSS px) mobile
                // layout viewport when a page has no `<meta viewport>`, which
                // puts page CSS in a different space from the View's pixels
                // (measured: a full-width heading's AX box was 2530 device px
                // in a 900 px View). ACOS's WebView contract is CSS px =
                // device px / density, so the engine lays out at that width.
                "mobile": false,
                "screenWidth": css_width,
                "screenHeight": css_height,
            }),
            self.browser.call_timeout,
        )?;
        self.call(
            "Emulation.setTouchEmulationEnabled",
            json!({"enabled": true, "maxTouchPoints": 5}),
            self.browser.call_timeout,
        )?;
        Ok(())
    }

    // --- navigation --------------------------------------------------------

    pub fn load_url(&self, url: &str) -> Result<Value> {
        let started_before = self.nav.inner.lock().expect("nav").started;
        let res = self.call(
            "Page.navigate",
            json!({"url": url}),
            self.browser.nav_timeout,
        )?;
        if let Some(err) = res.get("errorText").and_then(Value::as_str) {
            self.nav.fail("NAVIGATION_FAILED", err);
            return Err(Error::proto(
                code::NAVIGATION_FAILED,
                format!("{url}: {err}"),
            ));
        }
        let t0 = Instant::now();
        let (final_url, title) = self.nav.wait_load(started_before, self.browser.nav_timeout)?;
        Ok(json!({
            "url": final_url,
            "title": title,
            "loadMs": round1(t0.elapsed().as_secs_f64() * 1000.0),
        }))
    }

    pub fn load_data(&self, data: &str, mime: &str, encoding: &str, base_url: Option<&str>) -> Result<Value> {
        let url = match encoding {
            "base64" => format!("data:{mime};base64,{data}"),
            "utf8" | "utf-8" => {
                let b64 = base64_encode(data.as_bytes());
                format!("data:{mime};base64,{b64}")
            }
            other => {
                return Err(Error::proto(
                    code::BAD_REQUEST,
                    format!("encoding {other:?} (use utf8 or base64)"),
                ))
            }
        };
        let _ = base_url; // data URLs have no base; kept for wire compatibility
        self.load_url(&url)
    }

    pub fn reload(&self) -> Result<()> {
        self.call("Page.reload", json!({}), self.browser.call_timeout)?;
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.call("Page.stopLoading", json!({}), self.browser.call_timeout)?;
        Ok(())
    }

    pub fn set_user_agent(&self, ua: &str) -> Result<()> {
        self.call(
            "Emulation.setUserAgentOverride",
            json!({"userAgent": ua}),
            self.browser.call_timeout,
        )?;
        Ok(())
    }

    // --- v1.1: visibility, settings, engine scroll -------------------------

    /// `view.setVisibility`: a hidden View stops the renderer. DevTools has no
    /// visibility message; `Page.setWebLifecycleState` is the closest real
    /// engine action (the page is frozen while hidden, active again when shown,
    /// and `document.visibilityState` reports hidden). The broker never claims
    /// more than that.
    pub fn set_visibility(&self, visible: bool, window_visible: bool) -> Result<()> {
        let show = visible && window_visible;
        self.call(
            "Page.setWebLifecycleState",
            json!({"state": if show { "active" } else { "frozen" }}),
            self.browser.call_timeout,
        )?;
        Ok(())
    }

    /// `view.settings`: apply the fields the pinned engine can honour and name
    /// every field it cannot. A field is never silently dropped.
    pub fn apply_settings(&self, settings: &Value) -> Result<(Vec<String>, Vec<String>)> {
        let mut applied: Vec<String> = Vec::new();
        let mut rejected: Vec<String> = Vec::new();
        let Some(map) = settings.as_object() else {
            return Err(Error::proto(code::BAD_REQUEST, "settings must be an object"));
        };
        let needs_network = map.contains_key("cacheMode")
            || map.contains_key("loadsImagesAutomatically")
            || map.contains_key("blockNetworkImage");
        if needs_network {
            // `Network.setCacheDisabled` / `setBlockedURLs` need the domain;
            // enabling is idempotent.
            self.call("Network.enable", json!({}), self.browser.call_timeout)?;
        }
        let mut applied: Vec<String> = Vec::new();
        let mut rejected: Vec<String> = Vec::new();
        for (key, value) in map {
            match key.as_str() {
                "javascriptEnabled" => {
                    let enabled = value.as_bool().ok_or_else(|| {
                        Error::proto(code::BAD_REQUEST, "javascriptEnabled must be a boolean")
                    })?;
                    self.call(
                        "Emulation.setScriptExecutionDisabled",
                        json!({"value": !enabled}),
                        self.browser.call_timeout,
                    )?;
                    applied.push(key.clone());
                }
                "userAgent" => {
                    let ua = value.as_str().ok_or_else(|| {
                        Error::proto(code::BAD_REQUEST, "userAgent must be a string")
                    })?;
                    self.set_user_agent(ua)?;
                    applied.push(key.clone());
                }
                "cacheMode" => {
                    // Android: LOAD_DEFAULT=-1, LOAD_CACHE_ELSE_NETWORK=1,
                    // LOAD_NO_CACHE=2, LOAD_CACHE_ONLY=3. Only the
                    // "no cache" case maps to a DevTools switch; the others
                    // have no honest equivalent and are refused.
                    let mode = value.as_i64().unwrap_or(-1);
                    if mode == 2 {
                        applied.push(key.clone());
                    } else {
                        rejected.push(key.clone());
                    }
                }
                "loadsImagesAutomatically" | "blockNetworkImage" => {
                    let on = value.as_bool().ok_or_else(|| {
                        Error::proto(code::BAD_REQUEST, "blockNetworkImage must be a boolean")
                    })?;
                    // Both map to blocking image URLs. `loadsImagesAutomatically:
                    // false` is "do not load images"; `blockNetworkImage: true`
                    // is the same engine action.
                    let block = if key == "loadsImagesAutomatically" { !on } else { on };
                    let patterns = if block {
                        json!(["*.png", "*.jpg", "*.jpeg", "*.gif", "*.webp", "*.svg", "*.avif"])
                    } else {
                        json!([])
                    };
                    self.call(
                        "Network.setBlockedURLs",
                        json!({"urls": patterns}),
                        self.browser.call_timeout,
                    )?;
                    applied.push(key.clone());
                }
                _ => rejected.push(key.clone()),
            }
        }
        if map
            .get("cacheMode")
            .and_then(Value::as_i64)
            .map(|mode| mode == 2)
            .unwrap_or(false)
        {
            self.call(
                "Network.setCacheDisabled",
                json!({"cacheDisabled": true}),
                self.browser.call_timeout,
            )?;
        }
        Ok((applied, rejected))
    }

    /// `view.scrollTo`: move the page, in device pixels, and answer with the
    /// engine's own content/scroll metrics so the guest never invents one.
    pub fn scroll_to(&self, x: f64, y: f64) -> Result<Value> {
        let d = self.viewport().density;
        let _ = self.evaluate(
            &format!("window.scrollTo({}, {})", x / d, y / d),
            self.browser.call_timeout,
            false,
        )?;
        self.layout_metrics()
    }

    /// Content size and scroll offset from the engine, in display pixels.
    pub fn layout_metrics(&self) -> Result<Value> {
        let m = self.call("Page.getLayoutMetrics", json!({}), self.browser.call_timeout)?;
        let d = self.viewport().density;
        let content = &m["contentSize"];
        let vv = &m["visualViewport"];
        let f = |v: &Value, key: &str| v[key].as_f64().unwrap_or(0.0);
        Ok(json!({
            "contentWidth": round1(f(content, "width") * d),
            "contentHeight": round1(f(content, "height") * d),
            "scrollX": round1(f(vv, "pageX") * d),
            "scrollY": round1(f(vv, "pageY") * d),
        }))
    }

    /// `view.focusChanged` (v1.1): whether the page has a focused editable
    /// field, so the guest can answer `onCheckIsTextEditor` and open a real IME
    /// connection. The report is the page's own `document.activeElement`.
    pub fn focus_report(&self) -> Result<Value> {
        let script = r#"(() => {
            const a = document.activeElement;
            if (!a || a === document.body || a === document.documentElement) {
                return {focused: false, editable: false, type: '', tag: ''};
            }
            const tag = (a.tagName || '').toLowerCase();
            const type = (a.getAttribute && a.getAttribute('type')) || '';
            const nonText = ['button','submit','reset','checkbox','radio','range','file','color','image','hidden'];
            const editable = !!(a.isContentEditable || tag === 'textarea'
                || (tag === 'input' && nonText.indexOf(type.toLowerCase()) < 0));
            return {focused: true, editable: editable, type: type, tag: tag};
        })()"#;
        let res = self.call(
            "Runtime.evaluate",
            json!({"expression": script, "returnByValue": true}),
            self.browser.call_timeout,
        )?;
        Ok(res["result"]["value"].clone())
    }

    pub fn history(&self, action: &str) -> Result<Value> {
        let delta = match action {
            "back" => -1i64,
            "forward" => 1,
            "clear" => {
                return Err(Error::proto(
                    code::UNSUPPORTED,
                    "history.clear is not reachable through DevTools; the provider must \
                     replace the target (destroy + create) to clear history",
                ))
            }
            other => {
                return Err(Error::proto(
                    code::BAD_REQUEST,
                    format!("history action {other:?} (back|forward)"),
                ))
            }
        };
        let hist = self.call(
            "Page.getNavigationHistory",
            json!({}),
            self.browser.call_timeout,
        )?;
        let current = hist["currentIndex"].as_i64().unwrap_or(0);
        let target = current + delta;
        let entries = hist["entries"].as_array().cloned().unwrap_or_default();
        if target < 0 || target as usize >= entries.len() {
            return Err(Error::proto(
                code::NAVIGATION_FAILED,
                "no history entry in that direction",
            ));
        }
        let entry_id = entries[target as usize]["id"].as_i64().unwrap_or(target);
        self.call(
            "Page.navigateToHistoryEntry",
            json!({"entryId": entry_id}),
            self.browser.call_timeout,
        )?;
        Ok(self.nav.snapshot())
    }

    // --- javascript -------------------------------------------------------

    pub fn evaluate(&self, script: &str, timeout: Duration, user_gesture: bool) -> Result<Value> {
        let res = self.call(
            "Runtime.evaluate",
            json!({
                "expression": script,
                "returnByValue": true,
                "awaitPromise": true,
                "userGesture": user_gesture,
                "timeout": timeout.as_millis() as u64,
            }),
            timeout + Duration::from_secs(1),
        )?;
        if let Some(exc) = res.get("exceptionDetails") {
            let text = exc["exception"]["description"]
                .as_str()
                .or_else(|| exc["text"].as_str())
                .unwrap_or("javascript exception");
            return Err(Error::proto(code::JS_EXCEPTION, truncate(text, 2000)));
        }
        let result = &res["result"];
        let value_json = match result.get("value") {
            Some(v) => serde_json::to_string(v).map_err(|e| Error::proto(code::INTERNAL, e.to_string()))?,
            None => "null".to_string(),
        };
        Ok(json!({
            "valueJson": value_json,
            "type": result.get("type").and_then(Value::as_str).unwrap_or("undefined"),
        }))
    }

    /// Register a JS interface: binding + shim. The shim exposes only the
    /// methods the guest listed, so an unlisted method is a page `TypeError`
    /// (the `@JavascriptInterface` rule, enforced here rather than in the page).
    pub fn add_javascript_interface(&self, name: &str, methods: &[String]) -> Result<()> {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(Error::proto(
                code::BAD_REQUEST,
                format!("interface name {name:?} must be [A-Za-z0-9_]+"),
            ));
        }
        self.interfaces
            .lock()
            .expect("interfaces")
            .insert(name.to_string(), methods.to_vec());
        self.install_javascript_interface(name, methods)
    }

    /// Install (or re-install after a navigation) one interface's binding+shim.
    fn install_javascript_interface(&self, name: &str, methods: &[String]) -> Result<()> {
        let needs_binding = !self
            .bindings
            .lock()
            .expect("bindings")
            .iter()
            .any(|b| b == "__acos_jsi");
        if needs_binding {
            self.call(
                "Runtime.addBinding",
                json!({"name": "__acos_jsi"}),
                self.browser.call_timeout,
            )?;
            let mut bindings = self.bindings.lock().expect("bindings");
            if !bindings.iter().any(|b| b == "__acos_jsi") {
                bindings.push("__acos_jsi".to_string());
            }
        }
        // The shim helper itself is document state too; re-evaluate it before
        // installing the interface on a new document.
        self.evaluate(JSI_SHIM, self.browser.call_timeout, false)?;
        let methods_json = serde_json::to_string(methods)
            .map_err(|e| Error::proto(code::INTERNAL, e.to_string()))?;
        let script = format!(
            "window.__acos_jsi_install({}, {})",
            serde_json::to_string(name).map_err(|e| Error::proto(code::INTERNAL, e.to_string()))?,
            methods_json
        );
        self.evaluate(&script, self.browser.call_timeout, false)?;
        let mut bindings = self.bindings.lock().expect("bindings");
        if !bindings.iter().any(|b| b == name) {
            bindings.push(name.to_string());
        }
        Ok(())
    }

    /// A document loaded: every registered interface must exist in it again.
    fn reinstall_javascript_interfaces(&self) {
        let interfaces = self.interfaces.lock().expect("interfaces").clone();
        for (name, methods) in interfaces {
            if let Err(e) = self.install_javascript_interface(&name, &methods) {
                eprintln!("wv-jsi: reinstall {name} failed: {e}");
            }
        }
    }

    pub fn remove_javascript_interface(&self, name: &str) -> Result<()> {
        let script = format!(
            "delete window[{}]",
            serde_json::to_string(name).map_err(|e| Error::proto(code::INTERNAL, e.to_string()))?
        );
        let _ = self.evaluate(&script, self.browser.call_timeout, false);
        self.bindings
            .lock()
            .expect("bindings")
            .retain(|b| b != name);
        self.interfaces.lock().expect("interfaces").remove(name);
        Ok(())
    }

    /// Resolve (or reject) a page-side pending call created by the shim.
    pub fn resolve_js_call(&self, call_id: &str, result_json: Option<&str>, error: Option<&str>) -> Result<()> {
        let cid = serde_json::to_string(call_id).map_err(|e| Error::proto(code::INTERNAL, e.to_string()))?;
        let script = match (result_json, error) {
            (_, Some(err)) => format!(
                "window.__acos_jsi_resolve({cid}, null, {})",
                serde_json::to_string(err).map_err(|e| Error::proto(code::INTERNAL, e.to_string()))?
            ),
            (Some(r), None) => {
                let parsed: Value = serde_json::from_str(r).unwrap_or(Value::String(r.to_string()));
                format!(
                    "window.__acos_jsi_resolve({cid}, {}, null)",
                    serde_json::to_string(&serde_json::to_string(&parsed).unwrap_or_else(|_| "null".into()))
                        .map_err(|e| Error::proto(code::INTERNAL, e.to_string()))?
                )
            }
            (None, None) => format!("window.__acos_jsi_resolve({cid}, 'null', null)"),
        };
        self.evaluate(&script, self.browser.call_timeout, false)?;
        Ok(())
    }

    // --- frames ------------------------------------------------------------

    /// Deterministic frame: `HeadlessExperimental.beginFrame` renders exactly one
    /// compositor frame and screenshots it. PNG only in v1 (`WIRE.md` §4.2).
    pub fn render(&self, format: &str, timeout: Duration) -> Result<FrameData> {
        if format != "png" {
            return Err(Error::proto(
                code::UNSUPPORTED,
                format!("frame format {format:?}: v1 emits png (the guest decodes it into the View)"),
            ));
        }
        let t0 = Instant::now();
        // One beginFrame at a time across the whole browser (see Browser).
        let _render = self.browser.render_lock.lock().expect("render lock");
        // `beginFrame` can answer "not attached to an active page" while a
        // navigation is committing, and omits `screenshotData` when the
        // compositor produced no new frame. Both are transient: retry a few
        // times (bounded, no invented pixels) rather than hand the guest a
        // frame-less response or block it for the whole call timeout.
        let mut b64 = None;
        let mut has_damage = true;
        let mut last_error = String::new();
        let attempt_timeout = timeout.min(Duration::from_secs(2));
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(25));
            }
            match self.call(
                "HeadlessExperimental.beginFrame",
                json!({"screenshot": {"format": "png", "optimizeForSpeed": true}}),
                attempt_timeout,
            ) {
                Ok(res) => {
                    has_damage = res["hasDamage"].as_bool().unwrap_or(true);
                    if let Some(data) = res["screenshotData"].as_str() {
                        b64 = Some(data.to_string());
                        break;
                    }
                    last_error = format!(
                        "beginFrame returned no screenshotData (hasDamage={})",
                        res["hasDamage"].as_bool().unwrap_or(false)
                    );
                }
                Err(e) => {
                    last_error = e.to_string();
                    if !last_error.contains("Not attached to an active page") {
                        break;
                    }
                }
            }
        }
        let engine_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let b64 = b64.ok_or_else(|| Error::proto(code::FRAME_FAILED, last_error))?;
        let bytes = base64_decode(&b64)
            .ok_or_else(|| Error::proto(code::FRAME_FAILED, "frame is not valid base64"))?;
        let vp = self.viewport();
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(FrameData {
            seq,
            width: vp.width,
            height: vp.height,
            density: vp.density,
            format: FMT_PNG,
            stride: bytes.len() as u32,
            damage: if has_damage {
                (0, 0, vp.width, vp.height)
            } else {
                (0, 0, 0, 0)
            },
            has_damage,
            engine_ms,
            bytes,
        })
    }

    // --- accessibility -----------------------------------------------------

    pub fn ax(&self, max_nodes: Option<usize>, include_ignored: bool) -> Result<Value> {
        ax::export(self, max_nodes, include_ignored)
    }

    // --- input ------------------------------------------------------------

    /// Display pixels (what the wire uses, and what the AX export reports) to
    /// the CSS pixels DevTools input expects. Getting this wrong taps a point
    /// `density` times too far right and down — measured in the first gate run.
    fn to_css(&self, x: f64, y: f64) -> (f64, f64) {
        let d = self.viewport().density;
        (x / d, y / d)
    }

    fn touch_event(&self, kind: &str, points: Value) -> Result<crate::cdp::PendingReply> {
        self.send(
            "Input.dispatchTouchEvent",
            json!({"type": kind, "touchPoints": points}),
        )
    }

    /// Down → frame → up → frame. Under `--enable-begin-frame-control` DevTools
    /// only routes touch into the page when a compositor frame is produced, and
    /// the input acks may arrive after that frame; the acks are therefore
    /// waited for at the end, not per event.
    pub fn tap(&self, x: f64, y: f64, hold: Duration) -> Result<()> {
        let (cx, cy) = self.to_css(x, y);
        let down = self.touch_event(
            "touchStart",
            json!([{"x": cx, "y": cy, "radiusX": 4, "radiusY": 4, "force": 1, "id": 1}]),
        )?;
        let first = self.render("png", self.browser.frame_timeout)?;
        std::thread::sleep(hold);
        let up = self.touch_event("touchEnd", json!([]))?;
        let _second = self.render("png", self.browser.frame_timeout)?;
        for reply in [down, up] {
            let _ = reply.wait(Duration::from_secs(2));
        }
        let _ = first;
        Ok(())
    }

    pub fn touch(&self, action: &str, mut points: Value) -> Result<()> {
        let kind = match action {
            "down" => "touchStart",
            "move" => "touchMove",
            "up" => "touchEnd",
            "cancel" => "touchCancel",
            other => {
                return Err(Error::proto(
                    code::BAD_REQUEST,
                    format!("touch action {other:?} (down|move|up|cancel)"),
                ))
            }
        };
        if kind == "touchEnd" || kind == "touchCancel" {
            points = json!([]);
        } else if let Some(list) = points.as_array_mut() {
            let d = self.viewport().density;
            for p in list.iter_mut() {
                for key in ["x", "y", "radiusX", "radiusY"] {
                    if let Some(v) = p.get(key).and_then(Value::as_f64) {
                        p[key] = json!(v / d);
                    }
                }
            }
        }
        let reply = self.touch_event(kind, points)?;
        let _ = self.render("png", self.browser.frame_timeout)?;
        let _ = reply.wait(Duration::from_secs(2));
        Ok(())
    }

    pub fn scroll(&self, x: f64, y: f64, dx: f64, dy: f64) -> Result<()> {
        // Wheel deltas are CSS pixels too; the wire passes display pixels.
        let d = self.viewport().density;
        self.call(
            "Input.dispatchMouseEvent",
            json!({"type": "mouseWheel", "x": x / d, "y": y / d, "deltaX": dx / d, "deltaY": dy / d}),
            self.browser.call_timeout,
        )?;
        Ok(())
    }

    pub fn key(&self, action: &str, key: &str, code_name: &str, modifiers: &[String]) -> Result<()> {
        let mut mask = 0i64;
        for m in modifiers {
            mask |= match m.as_str() {
                "alt" => 1,
                "ctrl" | "control" => 2,
                "meta" | "cmd" => 4,
                "shift" => 8,
                other => {
                    return Err(Error::proto(
                        code::BAD_REQUEST,
                        format!("modifier {other:?} (alt|ctrl|meta|shift)"),
                    ))
                }
            };
        }
        let (vk, text) = key_info(key);
        let mut ev = json!({
            "key": key,
            "code": code_name,
            "windowsVirtualKeyCode": vk,
            "nativeVirtualKeyCode": vk,
            "modifiers": mask,
        });
        match action {
            "down" | "press" => {
                if let Some(t) = text {
                    ev["text"] = json!(t);
                    ev["unmodifiedText"] = json!(t);
                    ev["type"] = json!("keyDown");
                } else {
                    ev["type"] = json!("rawKeyDown");
                }
                self.call("Input.dispatchKeyEvent", ev.clone(), self.browser.call_timeout)?;
                if action == "press" {
                    ev["type"] = json!("keyUp");
                    ev["text"] = json!("");
                    self.call("Input.dispatchKeyEvent", ev, self.browser.call_timeout)?;
                }
            }
            "up" => {
                ev["type"] = json!("keyUp");
                self.call("Input.dispatchKeyEvent", ev, self.browser.call_timeout)?;
            }
            other => {
                return Err(Error::proto(
                    code::BAD_REQUEST,
                    format!("key action {other:?} (down|up|press)"),
                ))
            }
        }
        Ok(())
    }

    pub fn insert_text(&self, text: &str) -> Result<()> {
        self.call("Input.insertText", json!({"text": text}), self.browser.call_timeout)?;
        Ok(())
    }

    pub fn set_focus(&self, focused: bool) -> Result<()> {
        if !focused {
            self.evaluate("document.activeElement && document.activeElement.blur()", self.browser.call_timeout, false)?;
        } else {
            self.call("Emulation.setFocusEmulationEnabled", json!({"enabled": true}), self.browser.call_timeout)?;
        }
        Ok(())
    }

    // --- Fetch interception ------------------------------------------------

    pub fn fulfill(&self, request_id: &str, body: &[u8], status: u16, mime: &str, headers: Value) -> Result<()> {
        let mut hdrs = vec![json!({"name": "content-type", "value": mime})];
        if let Some(obj) = headers.as_object() {
            for (k, v) in obj {
                if let Some(v) = v.as_str() {
                    hdrs.push(json!({"name": k, "value": v}));
                }
            }
        }
        self.call(
            "Fetch.fulfillRequest",
            json!({
                "requestId": request_id,
                "responseCode": status,
                "responseHeaders": hdrs,
                "body": base64_encode(body),
            }),
            self.browser.call_timeout,
        )?;
        Ok(())
    }

    pub fn continue_request(&self, request_id: &str) -> Result<()> {
        self.call(
            "Fetch.continueRequest",
            json!({"requestId": request_id}),
            self.browser.call_timeout,
        )?;
        Ok(())
    }

    pub fn fail_request(&self, request_id: &str, reason: &str) -> Result<()> {
        let reason = if reason.starts_with("Failed") || reason.starts_with("Aborted") {
            reason.to_string()
        } else {
            format!("Failed: {reason}")
        };
        self.call(
            "Fetch.failRequest",
            json!({"requestId": request_id, "errorReason": reason.replace("net::", "")}),
            self.browser.call_timeout,
        )?;
        Ok(())
    }

    // --- event pump --------------------------------------------------------

    /// Drain the view's DevTools event queue once (blocking up to `wait`).
    /// Returns the derived events; navigation state is advanced here because
    /// this is the only consumer of the queue.
    pub fn pump_once(&self, wait: Duration) -> Vec<ViewEvent> {
        let mut out = Vec::new();
        let first = match self.events.wait_for(|_| true, wait) {
            Some(ev) => ev,
            None => return out,
        };
        let mut batch = vec![first];
        // Coalesce everything already queued so a burst of asset pauses does
        // not cost a thread wake-up each.
        batch.extend(self.events.drain_matching(|_| true));
        for ev in batch {
            if let Some(derived) = self.handle_event(ev) {
                out.push(derived);
            }
        }
        out
    }

    fn handle_event(&self, ev: Value) -> Option<ViewEvent> {
        let method = ev.get("method")?.as_str()?;
        let p = ev.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "__pipe_closed__" => None,
            "Page.frameRequestedNavigation" => {
                let url = p["url"].as_str().unwrap_or("").to_string();
                let is_redirect = p["reason"].as_str() == Some("redirect");
                self.nav.bump_started(&url);
                Some(ViewEvent::NavigationRequested {
                    url: url.clone(),
                    is_redirect,
                })
            }
            "Page.frameNavigated" => {
                let frame = &p["frame"];
                let url = frame["url"].as_str().unwrap_or("").to_string();
                let parent = frame.get("parentId").and_then(Value::as_str);
                let mut main = self.main_frame.lock().expect("main frame");
                if parent.is_none() {
                    *main = frame["id"].as_str().map(str::to_string);
                    drop(main);
                    if url.starts_with("about:") && url != "about:blank" {
                        return None;
                    }
                    return Some(ViewEvent::NavigationCommitted { url });
                }
                None
            }
            "Page.loadEventFired" => {
                let (url, title, back, forward) = match self.call(
                    "Runtime.evaluate",
                    json!({
                        "expression": "({u: location.href, t: document.title})",
                        "returnByValue": true,
                    }),
                    self.browser.call_timeout,
                ) {
                    Ok(res) => (
                        res["result"]["value"]["u"].as_str().unwrap_or("").to_string(),
                        res["result"]["value"]["t"].as_str().unwrap_or("").to_string(),
                        false,
                        false,
                    ),
                    Err(_) => (String::new(), String::new(), false, false),
                };
                self.nav.bump_finished(&url, &title, back, forward);
                // A new document dropped the JS-interface shim; re-install it
                // before the guest hears `navigation.finished`.
                self.reinstall_javascript_interfaces();
                Some(ViewEvent::NavigationFinished {
                    url,
                    title,
                    can_go_back: back,
                    can_go_forward: forward,
                })
            }
            "Page.loadingFailed" => {
                let failed = p["errorText"].as_str().unwrap_or("load failed").to_string();
                let url = self.nav.inner.lock().map(|g| g.last_url.clone()).unwrap_or_default();
                self.nav.fail("NAVIGATION_FAILED", &failed);
                Some(ViewEvent::NavigationFailed {
                    url,
                    code: failed,
                    message: "Page.loadingFailed".to_string(),
                })
            }
            "Page.frameStoppedLoading" => None,
            "Page.screencastFrame" => {
                let _ = self.call(
                    "Page.screencastFrameAck",
                    json!({"sessionId": p["sessionId"]}),
                    Duration::from_secs(5),
                );
                Some(ViewEvent::RenderInvalidated {
                    reason: "damage".to_string(),
                })
            }
            "Page.javascriptDialogOpening" => {
                // A modal dialog would wedge a headless page with no UI to show
                // it; auto-accept and record it as an invalidated render.
                let _ = self.call(
                    "Page.handleJavaScriptDialog",
                    json!({"accept": true, "promptText": ""}),
                    Duration::from_secs(5),
                );
                Some(ViewEvent::RenderInvalidated {
                    reason: "load".to_string(),
                })
            }
            "Runtime.bindingCalled" => {
                let name = p["name"].as_str().unwrap_or("").to_string();
                let payload = p["payload"].as_str().unwrap_or("").to_string();
                Some(ViewEvent::BindingCalled { name, payload })
            }
            "Fetch.requestPaused" => {
                let url = p["request"]["url"].as_str().unwrap_or("").to_string();
                Some(ViewEvent::RequestPaused {
                    request_id: p["requestId"].as_str().unwrap_or("").to_string(),
                    is_asset: url.starts_with("file:///android_asset/")
                        || url.starts_with("content://"),
                    url,
                    method: p["request"]["method"].as_str().unwrap_or("GET").to_string(),
                    resource_type: p["resourceType"].as_str().unwrap_or("Other").to_string(),
                    headers: p["request"]["headers"].clone(),
                })
            }
            _ => None,
        }
    }

    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let cdp = &self.browser.cdp;
        let _ = cdp.call(
            "Target.closeTarget",
            json!({"targetId": self.target_id}),
            None,
            Duration::from_secs(10),
        );
    }
}

/// DOM key name → (windows virtual key code, printable text).
fn key_info(key: &str) -> (i64, Option<&'static str>) {
    match key {
        "Enter" => (13, Some("\r")),
        "Tab" => (9, Some("\t")),
        "Backspace" => (8, None),
        "Delete" => (46, None),
        "Escape" => (27, None),
        " " => (32, Some(" ")),
        "ArrowLeft" => (37, None),
        "ArrowUp" => (38, None),
        "ArrowRight" => (39, None),
        "ArrowDown" => (40, None),
        "Home" => (36, None),
        "End" => (35, None),
        "PageUp" => (33, None),
        "PageDown" => (34, None),
        k if k.chars().count() == 1 => {
            let c = k.chars().next().unwrap_or(' ');
            (c.to_ascii_uppercase() as i64, None)
        }
        _ => (0, None),
    }
}

pub(crate) fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

// --- base64 (no dependency: the wire uses it for asset bodies and frames) ----

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut rev = [255u8; 256];
    for (i, c) in B64.iter().enumerate() {
        rev[*c as usize] = i as u8;
    }
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for quad in bytes.chunks(4) {
        let mut n = 0u32;
        let mut pad = 0;
        for (i, b) in quad.iter().enumerate() {
            let v = if *b == b'=' {
                pad += 1;
                0
            } else {
                let v = rev[*b as usize];
                if v == 255 {
                    return None;
                }
                v as u32
            };
            n |= v << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// A synthetic AX-facing helper used by the tests and by `ax::export`.
pub fn status_of(view: &View) -> Value {
    json!({
        "view": view.id,
        "targetId": view.target_id,
        "viewport": view.viewport(),
        "seq": view.seq.load(Ordering::SeqCst),
        "aliveMs": view.created.elapsed().as_millis() as u64,
    })
}

/// Accessibility export type re-exported so callers need one import.
pub type AxExport = (Vec<AxNode>, bool);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_validation_is_structured() {
        assert!(Viewport::new(1080, 2400, 2.625).is_ok());
        assert_eq!(
            Viewport::new(0, 10, 1.0).unwrap_err().code(),
            code::INVALID_VIEWPORT
        );
        assert_eq!(
            Viewport::new(10, 10, f64::NAN).unwrap_err().code(),
            code::INVALID_VIEWPORT
        );
    }

    #[test]
    fn base64_round_trips_binary() {
        for n in 0..64usize {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            let enc = base64_encode(&data);
            assert_eq!(base64_decode(&enc).as_deref(), Some(&data[..]), "n={n}");
        }
        assert!(base64_decode("!!!!").is_none());
        assert!(base64_decode("abc").is_none());
    }

    #[test]
    fn key_info_covers_text_and_named_keys() {
        assert_eq!(key_info("Enter"), (13, Some("\r")));
        assert_eq!(key_info("a").0, 'A' as i64);
        assert_eq!(key_info("F13").1, None);
    }
}
