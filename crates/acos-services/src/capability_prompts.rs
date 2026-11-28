//! v0.6.1: On-demand capability prompts for both interactive humans (`acos view`)
//! and agents (MCP / CLI).
//!
//! ACOS keeps optional modules `off` by default (ADR-0029) so idle memory and
//! boot latency stay unaffected. When an app hits a missing capability — an
//! unhandled `android.settings.*` intent, an unattached binder service
//! tombstone, or a captured external intent (such as a Google Play Store link)
//! — this broker records a structured, bounded [`CapabilityPrompt`] describing
//! what the app asked for and the exact explicit options to turn it on (or why
//! it is intentionally outside ACOS's scope).
//!
//! Modules are **never** auto-attached: every enable action is an explicit user
//! click or agent tool call, preserving zero-false-pass guarantees.

use std::collections::VecDeque;

use serde_json::{json, Value};

use crate::ams::IntentFacts;
use crate::intents::CapturedIntent;
use crate::module::{AttachMode, Profile};

/// Maximum prompts retained in a session's ring buffer.
pub const MAX_PROMPTS: usize = 32;
/// Maximum failed intents retained for one-click / agent replay after attach.
pub const MAX_SAVED_INTENTS: usize = 16;
/// Deduplication window for identical binder tombstone hits (ms).
const TOMBSTONE_DEDUP_MS: u64 = 3_000;

/// What triggered a [`CapabilityPrompt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTrigger {
    /// An `android.settings.*` intent failed with `START_NOT_FOUND` while
    /// `system.settings` was not installed/attached.
    IntentNotFound,
    /// A binder transaction hit an unattached module's tombstone.
    ServiceTombstone,
    /// An implicit external intent (`VIEW`, `SEND`, `PICK`, `GET_CONTENT`) was
    /// captured by [`crate::intents::IntentState`].
    IntentCaptured,
}

impl PromptTrigger {
    /// Wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IntentNotFound => "intent_not_found",
            Self::ServiceTombstone => "service_tombstone",
            Self::IntentCaptured => "intent_captured",
        }
    }
}

/// How an option can be acted upon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptOptionKind {
    /// Can be enabled immediately on the live session with no restart.
    HotAttach,
    /// Requires relaunching the session with the named module(s).
    RelaunchRequired,
    /// Informational / non-mutating (e.g. copy captured URL or `--extra-apk` hint).
    InfoOnly,
    /// Interactive file/photo upload fulfilling a captured picker intent via
    /// `content://com.acos.picker/...`.
    FilePicker,
    /// Cancel a captured picker intent (`RESULT_CANCELED`).
    CancelPicker,
}

impl PromptOptionKind {
    /// Wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HotAttach => "hot_attach",
            Self::RelaunchRequired => "relaunch_required",
            Self::InfoOnly => "info_only",
            Self::FilePicker => "file_picker",
            Self::CancelPicker => "cancel_picker",
        }
    }
}

/// One module attach step inside a [`PromptOption`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleAttachSpec {
    /// Module id (e.g. `"system.settings"`, `"accessibility"`).
    pub id: String,
    /// Profile name (e.g. `"real"`, `"scripted"`).
    pub profile: String,
}

impl ModuleAttachSpec {
    /// Serialize for MCP/CLI/live-view wire JSON.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "profile": self.profile,
        })
    }
}

/// Optional follow-up module control call (e.g. `accessibility.enable`).
#[derive(Debug, Clone, PartialEq)]
pub struct ControlSpec {
    /// Module id.
    pub module: String,
    /// Module control op name.
    pub op: String,
    /// Control arguments JSON.
    pub args: Value,
}

impl ControlSpec {
    /// Serialize for MCP/CLI/live-view wire JSON.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "module": self.module,
            "op": self.op,
            "args": self.args,
        })
    }
}

/// One explicit action presented to a human or agent.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptOption {
    /// Stable option id (e.g. `"enable_and_open_settings"`).
    pub id: String,
    /// Button / menu label for interactive UI.
    pub label: String,
    /// Whether this can run live, needs a relaunch, or is info-only.
    pub kind: PromptOptionKind,
    /// Modules to attach in order.
    pub modules: Vec<ModuleAttachSpec>,
    /// Optional module control op to run after attach.
    pub control: Option<ControlSpec>,
    /// Saved intent id to replay after attaching modules.
    pub replay_intent: Option<u64>,
    /// Optional URL (for captured `VIEW` intents).
    pub url: Option<String>,
    /// Captured intent sequence number (for `FilePicker` / `CancelPicker`).
    pub capture_seq: Option<u64>,
    /// Optional MIME filter hint for `<input type="file" accept="...">`.
    pub accept: Option<String>,
}

impl PromptOption {
    /// Serialize for MCP/CLI/live-view wire JSON.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("id".into(), json!(self.id));
        obj.insert("label".into(), json!(self.label));
        obj.insert("kind".into(), json!(self.kind.as_str()));
        if !self.modules.is_empty() {
            obj.insert(
                "modules".into(),
                json!(self.modules.iter().map(ModuleAttachSpec::to_json).collect::<Vec<_>>()),
            );
        }
        if let Some(control) = &self.control {
            obj.insert("control".into(), control.to_json());
        }
        if let Some(replay_id) = self.replay_intent {
            obj.insert("replay_intent".into(), json!(replay_id));
        }
        if let Some(url) = &self.url {
            obj.insert("url".into(), json!(url));
        }
        if let Some(seq) = self.capture_seq {
            obj.insert("capture_seq".into(), json!(seq));
        }
        if let Some(accept) = &self.accept {
            obj.insert("accept".into(), json!(accept));
        }
        Value::Object(obj)
    }
}

/// One capability-miss or external-intent prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityPrompt {
    /// Monotonic 1-based sequence number per session.
    pub seq: u64,
    /// Host wall clock timestamp (ms since epoch).
    pub utc_ms: u64,
    /// Why this prompt was emitted.
    pub trigger: PromptTrigger,
    /// Exact intent action / service name / captured URI the app requested.
    pub request: String,
    /// Short headline for interactive display.
    pub title: String,
    /// Plain explanation naming what is off and how to enable or test it.
    pub message: String,
    /// Explicit actions available to the human or agent.
    pub options: Vec<PromptOption>,
}

impl CapabilityPrompt {
    /// Serialize for MCP/CLI/live-view wire JSON.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "seq": self.seq,
            "utc_ms": self.utc_ms,
            "trigger": self.trigger.as_str(),
            "request": self.request,
            "title": self.title,
            "message": self.message,
            "options": self.options.iter().map(PromptOption::to_json).collect::<Vec<_>>(),
        })
    }
}

/// A failed `startActivity` intent saved so it can be replayed once the needed
/// module (`system.settings`) is hot-attached.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedIntent {
    /// Monotonic 1-based saved intent id.
    pub id: u64,
    /// Original parsed intent facts.
    pub intent: IntentFacts,
    /// Original `startFlags`.
    pub flags: i32,
    /// Original `resultTo` binder token.
    pub result_to: u64,
    /// Original `requestCode`.
    pub request_code: i32,
    /// Original `resultWho`.
    pub result_who: Option<String>,
    /// Original calling process tag.
    pub caller_tag: Option<u32>,
}

/// Whether `com.android.settings` (`system.settings:real`) handles `action`.
#[must_use]
pub fn settings_action_supported(action: &str) -> bool {
    matches!(
        action,
        "android.settings.SETTINGS"
            | "android.settings.WIFI_SETTINGS"
            | "android.settings.APPLICATION_DETAILS_SETTINGS"
            | "android.settings.ACCESSIBILITY_SETTINGS"
            | "android.settings.action.MANAGE_OVERLAY_PERMISSION"
            | "android.settings.APP_NOTIFICATION_SETTINGS"
    )
}

/// Short human-readable title for a supported `android.settings.*` action.
#[must_use]
pub fn settings_action_title(action: &str) -> &'static str {
    match action {
        "android.settings.ACCESSIBILITY_SETTINGS" => "Accessibility Settings",
        "android.settings.WIFI_SETTINGS" => "Wi-Fi Settings",
        "android.settings.APPLICATION_DETAILS_SETTINGS" => "App Info Settings",
        "android.settings.action.MANAGE_OVERLAY_PERMISSION" => "Display Over Other Apps",
        "android.settings.APP_NOTIFICATION_SETTINGS" => "App Notification Settings",
        _ => "System Settings",
    }
}

/// Extract the target Android package name from a Play Store or `market://` URI.
///
/// Recognizes:
/// - `https://play.google.com/store/apps/details?id=com.ichi2.anki`
/// - `http://play.google.com/store/apps/details?id=com.ichi2.anki&...`
/// - `market://details?id=com.ichi2.anki`
#[must_use]
pub fn parse_store_package(uri: &str) -> Option<String> {
    let trimmed = uri.trim();
    let is_store = trimmed.starts_with("market://details")
        || trimmed.starts_with("https://play.google.com/store/apps/details")
        || trimmed.starts_with("http://play.google.com/store/apps/details");
    if !is_store {
        return None;
    }
    let query = trimmed.split_once('?')?.1;
    let query = query.split('#').next().unwrap_or(query);
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            if key == "id" && !value.is_empty() && is_valid_package_id(value) {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn is_valid_package_id(value: &str) -> bool {
    value.contains('.')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_'))
}

/// Per-session bounded buffer of capability prompts and replayable intents.
#[derive(Debug, Default)]
pub struct CapabilityPromptState {
    next_seq: u64,
    next_intent_id: u64,
    prompts: VecDeque<CapabilityPrompt>,
    saved_intents: VecDeque<SavedIntent>,
}

impl CapabilityPromptState {
    /// Create an empty state (zero allocations until a prompt is recorded).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The latest prompt sequence number (0 if none recorded yet).
    #[must_use]
    pub fn cursor(&self) -> u64 {
        self.next_seq
    }

    /// Prompts recorded strictly after `after_seq`, oldest first.
    #[must_use]
    pub fn since(&self, after_seq: u64) -> Vec<CapabilityPrompt> {
        self.prompts
            .iter()
            .filter(|p| p.seq > after_seq)
            .cloned()
            .collect()
    }

    /// Up to `limit` most recent prompts, oldest first.
    #[must_use]
    pub fn recent(&self, limit: usize) -> Vec<CapabilityPrompt> {
        if limit == 0 || self.prompts.is_empty() {
            return Vec::new();
        }
        let skip = self.prompts.len().saturating_sub(limit);
        self.prompts.iter().skip(skip).cloned().collect()
    }

    /// Look up a saved intent by id for replay.
    #[must_use]
    pub fn saved_intent(&self, intent_id: u64) -> Option<SavedIntent> {
        self.saved_intents
            .iter()
            .find(|s| s.id == intent_id)
            .cloned()
    }

    fn push_prompt(&mut self, prompt: CapabilityPrompt) -> CapabilityPrompt {
        while self.prompts.len() >= MAX_PROMPTS {
            self.prompts.pop_front();
        }
        self.prompts.push_back(prompt.clone());
        prompt
    }

    fn save_intent(
        &mut self,
        intent: &IntentFacts,
        flags: i32,
        result_to: u64,
        request_code: i32,
        result_who: Option<String>,
        caller_tag: Option<u32>,
    ) -> u64 {
        self.next_intent_id += 1;
        let id = self.next_intent_id;
        while self.saved_intents.len() >= MAX_SAVED_INTENTS {
            self.saved_intents.pop_front();
        }
        self.saved_intents.push_back(SavedIntent {
            id,
            intent: intent.clone(),
            flags,
            result_to,
            request_code,
            result_who,
            caller_tag,
        });
        id
    }

    /// Record an unhandled `android.settings.*` intent that `system.settings`
    /// can satisfy once attached.
    #[allow(clippy::too_many_arguments)]
    pub fn record_settings_intent_miss(
        &mut self,
        intent: &IntentFacts,
        flags: i32,
        result_to: u64,
        request_code: i32,
        result_who: Option<String>,
        caller_tag: Option<u32>,
        installed_a11y_services: &[String],
        now_ms: u64,
    ) -> CapabilityPrompt {
        let action = intent
            .action
            .as_deref()
            .unwrap_or("android.settings.SETTINGS");
        let intent_id = self.save_intent(
            intent,
            flags,
            result_to,
            request_code,
            result_who,
            caller_tag,
        );
        let screen = settings_action_title(action);
        let is_a11y = action == "android.settings.ACCESSIBILITY_SETTINGS";

        let mut options = Vec::new();
        if is_a11y {
            options.push(PromptOption {
                id: "enable_and_open_settings".into(),
                label: "Turn on & Open Settings".into(),
                kind: PromptOptionKind::HotAttach,
                modules: vec![
                    ModuleAttachSpec {
                        id: crate::modules::settings::MODULE_ID.into(),
                        profile: Profile::Real.as_str().into(),
                    },
                    ModuleAttachSpec {
                        id: crate::modules::accessibility::MODULE_ID.into(),
                        profile: Profile::Real.as_str().into(),
                    },
                ],
                control: None,
                replay_intent: Some(intent_id),
                url: None,
                capture_seq: None,
                accept: None,
            });
            if !installed_a11y_services.is_empty() {
                options.push(PromptOption {
                    id: "enable_accessibility_direct".into(),
                    label: "Enable Accessibility directly".into(),
                    kind: PromptOptionKind::HotAttach,
                    modules: vec![ModuleAttachSpec {
                        id: crate::modules::accessibility::MODULE_ID.into(),
                        profile: Profile::Real.as_str().into(),
                    }],
                    control: Some(ControlSpec {
                        module: crate::modules::accessibility::MODULE_ID.into(),
                        op: "enable".into(),
                        args: json!({ "services": installed_a11y_services }),
                    }),
                    replay_intent: None,
                    url: None,
                    capture_seq: None,
                    accept: None,
                });
            }
        } else {
            options.push(PromptOption {
                id: "enable_and_open_settings".into(),
                label: "Turn on & Open Settings".into(),
                kind: PromptOptionKind::HotAttach,
                modules: vec![ModuleAttachSpec {
                    id: crate::modules::settings::MODULE_ID.into(),
                    profile: Profile::Real.as_str().into(),
                }],
                control: None,
                replay_intent: Some(intent_id),
                url: None,
                capture_seq: None,
                accept: None,
            });
        }

        let message = if is_a11y {
            format!(
                "App requested {action}. Turn on `system.settings` and `accessibility` to open {screen} and toggle accessibility services."
            )
        } else {
            format!(
                "App requested {action}. Turn on `system.settings` to open {screen}."
            )
        };

        self.next_seq += 1;
        let prompt = CapabilityPrompt {
            seq: self.next_seq,
            utc_ms: now_ms,
            trigger: PromptTrigger::IntentNotFound,
            request: action.to_string(),
            title: format!("{screen} is turned off"),
            message,
            options,
        };
        self.push_prompt(prompt)
    }

    /// Record a hit on an unattached module's binder tombstone, deduplicating
    /// rapid repeats for the same module within [`TOMBSTONE_DEDUP_MS`].
    pub fn record_tombstone_hit(
        &mut self,
        service: &str,
        module_id: &str,
        attach_mode: AttachMode,
        default_profile: Profile,
        now_ms: u64,
    ) -> Option<CapabilityPrompt> {
        let request = format!("{service} ({module_id})");
        if let Some(last) = self.prompts.iter().rev().find(|p| {
            p.trigger == PromptTrigger::ServiceTombstone && p.request == request
        }) {
            if now_ms.saturating_sub(last.utc_ms) < TOMBSTONE_DEDUP_MS {
                return None;
            }
        }

        let kind = match attach_mode {
            AttachMode::Hot => PromptOptionKind::HotAttach,
            AttachMode::AtLaunch => PromptOptionKind::RelaunchRequired,
        };
        let label = match attach_mode {
            AttachMode::Hot => format!("Turn on {module_id}"),
            AttachMode::AtLaunch => format!("Requires relaunch with --service {module_id}:{}", default_profile.as_str()),
        };
        let message = match attach_mode {
            AttachMode::Hot => format!(
                "App called Android service `{service}`, which is off. Turn on `{module_id}` ({}) to enable it live.",
                default_profile.as_str()
            ),
            AttachMode::AtLaunch => format!(
                "App called Android service `{service}`, which is off. Relaunch with `--service {module_id}:{}` to enable it.",
                default_profile.as_str()
            ),
        };

        self.next_seq += 1;
        let prompt = CapabilityPrompt {
            seq: self.next_seq,
            utc_ms: now_ms,
            trigger: PromptTrigger::ServiceTombstone,
            request,
            title: format!("Module `{module_id}` is turned off"),
            message,
            options: vec![PromptOption {
                id: format!("attach_{}", module_id.replace('.', "_")),
                label,
                kind,
                modules: vec![ModuleAttachSpec {
                    id: module_id.to_string(),
                    profile: default_profile.as_str().to_string(),
                }],
                control: None,
                replay_intent: None,
                url: None,
                capture_seq: None,
                accept: None,
            }],
        };
        Some(self.push_prompt(prompt))
    }

    /// Record an external intent captured by [`crate::intents::IntentState`].
    pub fn record_captured_intent(&mut self, record: &CapturedIntent) -> CapabilityPrompt {
        let data = record.data.as_deref().unwrap_or("");
        // A plain `startActivity` has no result recipient, so neither choosing
        // a file nor cancelling could ever reach the app.
        let (title, message, options) = if record.is_picker() && record.for_result {
            let accept = record.picker_accept_hint();
            let mime_desc = accept.as_deref().unwrap_or("*/*");
            let eff_action = record.effective_action();
            let title = if record.is_create_document() {
                "Save Document requested".to_string()
            } else {
                "File / Photo Picker requested".to_string()
            };
            let message = format!(
                "App requested `{eff_action}` (type `{mime_desc}`). Choose a host file to return as `content://com.acos.picker/...`, or cancel the picker."
            );
            (
                title,
                message,
                vec![
                    PromptOption {
                        id: "pick_file".into(),
                        label: "Choose file…".into(),
                        kind: PromptOptionKind::FilePicker,
                        modules: Vec::new(),
                        control: None,
                        replay_intent: None,
                        url: None,
                        capture_seq: Some(record.seq),
                        accept,
                    },
                    PromptOption {
                        id: "cancel_picker".into(),
                        label: "Cancel picker".into(),
                        kind: PromptOptionKind::CancelPicker,
                        modules: Vec::new(),
                        control: None,
                        replay_intent: None,
                        url: None,
                        capture_seq: Some(record.seq),
                        accept: None,
                    },
                ],
            )
        } else if let Some(pkg) = parse_store_package(data) {
            (
                format!("Store link captured ({pkg})"),
                format!(
                    "App tried to open Google Play for `{pkg}`. ACOS has no Play Store — relaunch with `--extra-apk <{pkg}.apk>` to test with it installed."
                ),
                vec![PromptOption {
                    id: "copy_url".into(),
                    label: "Copy store URL".into(),
                    kind: PromptOptionKind::InfoOnly,
                    modules: Vec::new(),
                    control: None,
                    replay_intent: None,
                    url: Some(data.to_string()),
                    capture_seq: None,
                    accept: None,
                }],
            )
        } else {
            let target_desc = if !data.is_empty() {
                data.to_string()
            } else if let Some(mime) = record.mime_type.as_deref() {
                mime.to_string()
            } else {
                "(no data)".to_string()
            };
            let mut opts = Vec::new();
            if !data.is_empty() {
                opts.push(PromptOption {
                    id: "copy_url".into(),
                    label: "Copy URI".into(),
                    kind: PromptOptionKind::InfoOnly,
                    modules: Vec::new(),
                    control: None,
                    replay_intent: None,
                    url: Some(data.to_string()),
                    capture_seq: None,
                    accept: None,
                });
            }
            (
                "External intent captured".into(),
                format!(
                    "Captured {} for `{target_desc}` (no external handler installed in this session).",
                    record.action
                ),
                opts,
            )
        };

        let request = if data.is_empty() {
            record.effective_action().to_string()
        } else {
            format!("{} {data}", record.effective_action())
        };

        self.next_seq += 1;
        let prompt = CapabilityPrompt {
            seq: self.next_seq,
            utc_ms: record.utc_ms,
            trigger: PromptTrigger::IntentCaptured,
            request,
            title,
            message,
            options,
        };
        self.push_prompt(prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn parses_play_store_and_market_uris() {
        assert_eq!(
            parse_store_package("https://play.google.com/store/apps/details?id=com.ichi2.anki"),
            Some("com.ichi2.anki".into())
        );
        assert_eq!(
            parse_store_package("market://details?id=com.ichi2.anki&referrer=gaten"),
            Some("com.ichi2.anki".into())
        );
        assert_eq!(
            parse_store_package("https://example.com/?id=com.ichi2.anki"),
            None
        );
        assert_eq!(
            parse_store_package("https://play.google.com/store/apps/details?id=notapackage"),
            None
        );
    }

    #[test]
    fn accessibility_settings_miss_emits_two_hot_options_and_saves_intent() {
        let mut state = CapabilityPromptState::new();
        let intent = IntentFacts {
            action: Some("android.settings.ACCESSIBILITY_SETTINGS".into()),
            ..IntentFacts::default()
        };
        let prompt = state.record_settings_intent_miss(
            &intent,
            0,
            0,
            -1,
            None,
            Some(0),
            &["com.gaten/.GatenAccessibilityService".into()],
            1_000,
        );
        assert_eq!(prompt.seq, 1);
        assert_eq!(prompt.trigger, PromptTrigger::IntentNotFound);
        assert_eq!(prompt.options.len(), 2);
        assert_eq!(prompt.options[0].id, "enable_and_open_settings");
        assert_eq!(prompt.options[0].kind, PromptOptionKind::HotAttach);
        assert_eq!(prompt.options[0].replay_intent, Some(1));
        assert_eq!(prompt.options[1].id, "enable_accessibility_direct");
        assert!(state.saved_intent(1).is_some());
        assert_eq!(state.since(0).len(), 1);
        assert!(state.since(1).is_empty());
    }

    #[test]
    fn tombstone_hits_deduplicate_within_window() {
        let mut state = CapabilityPromptState::new();
        let first = state.record_tombstone_hit(
            "location",
            "location",
            AttachMode::Hot,
            Profile::Scripted,
            1_000,
        );
        assert!(first.is_some());
        let dup = state.record_tombstone_hit(
            "location",
            "location",
            AttachMode::Hot,
            Profile::Scripted,
            2_500,
        );
        assert!(dup.is_none());
        let later = state.record_tombstone_hit(
            "location",
            "location",
            AttachMode::Hot,
            Profile::Scripted,
            4_500,
        );
        assert!(later.is_some());
        assert_eq!(state.cursor(), 2);
    }

    #[test]
    fn captured_store_intent_names_package_and_extra_apk_hint() {
        let mut state = CapabilityPromptState::new();
        let captured = CapturedIntent {
            seq: 1,
            utc_ms: 5_000,
            action: "android.intent.action.VIEW".into(),
            wrapped_action: None,
            data: Some("https://play.google.com/store/apps/details?id=com.ichi2.anki".into()),
            mime_type: None,
            wrapped_mime_type: None,
            categories: Vec::new(),
            extras: BTreeMap::new(),
            component: "com.acos.capture/.Capture".into(),
            flags: 0,
            request_code: -1,
            for_result: false,
            scripted_result: None,
        };
        let prompt = state.record_captured_intent(&captured);
        assert_eq!(prompt.trigger, PromptTrigger::IntentCaptured);
        assert!(prompt.title.contains("com.ichi2.anki"));
        assert!(prompt.message.contains("--extra-apk"));
        assert_eq!(prompt.options[0].kind, PromptOptionKind::InfoOnly);
        assert_eq!(
            prompt.options[0].url.as_deref(),
            Some("https://play.google.com/store/apps/details?id=com.ichi2.anki")
        );
    }

    #[test]
    fn captured_picker_intent_emits_file_picker_and_cancel_options() {
        let mut state = CapabilityPromptState::new();
        let captured = CapturedIntent {
            seq: 3,
            utc_ms: 9_000,
            action: "android.intent.action.OPEN_DOCUMENT".into(),
            wrapped_action: None,
            data: None,
            mime_type: Some("image/*".into()),
            wrapped_mime_type: None,
            categories: vec!["android.intent.category.OPENABLE".into()],
            extras: BTreeMap::new(),
            component: "com.acos.capture/.Capture".into(),
            flags: 0,
            request_code: 42,
            for_result: true,
            scripted_result: None,
        };
        let prompt = state.record_captured_intent(&captured);
        assert_eq!(prompt.trigger, PromptTrigger::IntentCaptured);
        assert_eq!(prompt.title, "File / Photo Picker requested");
        assert_eq!(prompt.options.len(), 2);
        assert_eq!(prompt.options[0].kind, PromptOptionKind::FilePicker);
        assert_eq!(prompt.options[0].capture_seq, Some(3));
        assert_eq!(prompt.options[0].accept.as_deref(), Some("image/*"));
        assert_eq!(prompt.options[1].kind, PromptOptionKind::CancelPicker);
        assert_eq!(prompt.options[1].capture_seq, Some(3));

        let no_result = CapturedIntent {
            seq: 4,
            request_code: -1,
            for_result: false,
            ..captured
        };
        let prompt = state.record_captured_intent(&no_result);
        assert_eq!(prompt.title, "External intent captured");
        assert!(
            !prompt.options.iter().any(|o| matches!(
                o.kind,
                PromptOptionKind::FilePicker | PromptOptionKind::CancelPicker
            )),
            "a start without a result must not offer picker actions"
        );
    }
}
