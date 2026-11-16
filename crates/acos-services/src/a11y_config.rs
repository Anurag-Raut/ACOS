//! The `<accessibility-service>` meta-data block of a declared service.
//!
//! AOSP does not read the accessibility service's capabilities from the
//! manifest: `PackageManagerService` stores the `<service>`'s
//! `<meta-data android:name="android.accessibilityservice"
//! android:resource="@xml/..."/>` as a **resource id**, and
//! `AccessibilityServiceInfo.parseServiceInfo` inflates that XML and reads it
//! (`base/core/java/android/accessibilityservice/AccessibilityServiceInfo.java`
//! :735-800, the `AccessibilityService_*` styleable):
//!
//! ```text
//! canRetrieveWindowContent=true  -> CAPABILITY_CAN_RETRIEVE_WINDOW_CONTENT (1)
//! canPerformGestures=true        -> CAPABILITY_CAN_PERFORM_GESTURES       (1 << 5)
//! canTakeScreenshot=true         -> CAPABILITY_CAN_TAKE_SCREENSHOT        (1 << 7)
//! accessibilityFlags=…           -> FLAG_* bits (FLAG_RETRIEVE_INTERACTIVE_WINDOWS = 1 << 6)
//! ```
//!
//! and `AccessibilitySecurityPolicy` gates the whole interaction surface on
//! them:
//!
//! ```text
//! canRetrieveWindowContentLocked = capabilities & CAPABILITY_CAN_RETRIEVE_WINDOW_CONTENT
//! canRetrieveWindowsLocked       = canRetrieveWindowContentLocked
//!                                  && mRetrieveInteractiveWindows   (FLAG_RETRIEVE_INTERACTIVE_WINDOWS)
//! canGetAccessibilityNodeInfoLocked = canRetrieveWindowContentLocked && window is known/active
//! ```
//!
//! (`AccessibilitySecurityPolicy.java:317-346`.)
//!
//! This module is the missing glue between the two: resource id -> resource
//! table entry -> file path -> APK zip entry -> binary XML -> typed config.

use acos_apk::{Apk, ResValue, ResourceTable, TypedValue, XmlDocument, XmlNode};

use crate::error::{Result, ServiceError};

/// `AccessibilityServiceInfo.CAPABILITY_CAN_RETRIEVE_WINDOW_CONTENT`.
pub const CAPABILITY_CAN_RETRIEVE_WINDOW_CONTENT: i32 = 1;
/// `AccessibilityServiceInfo.CAPABILITY_CAN_PERFORM_GESTURES`.
pub const CAPABILITY_CAN_PERFORM_GESTURES: i32 = 1 << 5;
/// `AccessibilityServiceInfo.CAPABILITY_CAN_TAKE_SCREENSHOT`.
pub const CAPABILITY_CAN_TAKE_SCREENSHOT: i32 = 1 << 7;
/// `AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS` (`1 << 6`).
///
/// MODA11Y5 guessed `0x2`; the image's `AccessibilityServiceInfo.java:315`
/// says `1 << 6`, and `AbstractAccessibilityServiceConnection.java:453-454`
/// reads exactly that bit into `mRetrieveInteractiveWindows`.
pub const FLAG_RETRIEVE_INTERACTIVE_WINDOWS: i32 = 1 << 6;

/// The `android:accessibilityservice` meta-data name a service declares it in
/// (`AccessibilityService.SERVICE_META_DATA`).
pub const SERVICE_META_DATA: &str = "android.accessibilityservice";

/// What one `<accessibility-service>` block declares.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct A11yServiceConfig {
    /// `android:accessibilityEventTypes` (`AccessibilityEvent` type mask).
    pub event_types: i32,
    /// `android:accessibilityFeedbackType`.
    pub feedback_type: i32,
    /// `android:notificationTimeout`.
    pub notification_timeout: i64,
    /// `android:accessibilityFlags` (`FLAG_*` mask).
    pub flags: i32,
    /// The `can…` attributes, as `AccessibilityServiceInfo.getCapabilities`.
    pub capabilities: i32,
}

impl A11yServiceConfig {
    /// `AccessibilitySecurityPolicy.canRetrieveWindowContentLocked`.
    #[must_use]
    pub const fn can_retrieve_window_content(&self) -> bool {
        self.capabilities & CAPABILITY_CAN_RETRIEVE_WINDOW_CONTENT != 0
    }

    /// `AccessibilitySecurityPolicy.canRetrieveWindowsLocked`: a service sees
    /// the window *list* only when it may read window content **and** asked for
    /// interactive windows.
    #[must_use]
    pub const fn can_retrieve_windows(&self) -> bool {
        self.can_retrieve_window_content()
            && self.flags & FLAG_RETRIEVE_INTERACTIVE_WINDOWS != 0
    }

    /// `AccessibilitySecurityPolicy.canPerformGestures`.
    #[must_use]
    pub const fn can_perform_gestures(&self) -> bool {
        self.capabilities & CAPABILITY_CAN_PERFORM_GESTURES != 0
    }

    /// `AccessibilitySecurityPolicy.canTakeScreenshot`.
    #[must_use]
    pub const fn can_take_screenshot(&self) -> bool {
        self.capabilities & CAPABILITY_CAN_TAKE_SCREENSHOT != 0
    }

    /// Read the block an `<meta-data android:name="android.accessibilityservice"
    /// android:resource="@xml/…"/>` points at.
    ///
    /// The reference is resolved through the APK's own resource table
    /// (`res/xml/<name>.xml`), the entry is read from the zip and parsed as
    /// binary XML. A reference that does not resolve to a file, or a document
    /// whose root is not `<accessibility-service>`, is a structured error: a
    /// service whose declaration cannot be read has **no** capabilities, never
    /// invented ones.
    pub fn from_apk(apk: &Apk, resource_id: u32) -> Result<Self> {
        let table = apk
            .resources()
            .map_err(|e| ServiceError::Internal(format!("resources.arsc: {e}")))?
            .ok_or_else(|| {
                ServiceError::Internal(format!(
                    "the APK has no resource table, so @{resource_id:#010x} cannot be resolved"
                ))
            })?;
        let path = resolve_file_path(&table, resource_id).ok_or_else(|| {
            ServiceError::Internal(format!(
                "@{resource_id:#010x} does not resolve to a resource file"
            ))
        })?;
        let bytes = apk.entry_bytes(&path).ok_or_else(|| {
            ServiceError::Internal(format!("the APK has no entry {path:?}"))
        })?;
        let document = acos_apk::axml::parse(&bytes)
            .map_err(|e| ServiceError::Internal(format!("{path}: binary XML: {e}")))?;
        let mut config = Self::from_document(&document)?;
        // A `TypedValue::Reference` on an attribute (`android:accessibilityFlags=
        // "@integer/x"`) is resolved through the same table, one level deep —
        // AAPT inlines plain literals, which is the normal case.
        config.resolve_references(&table, &document);
        Ok(config)
    }

    /// Read the block from an already-parsed binary XML document.
    ///
    /// Attribute names are the styleable's own (`canRetrieveWindowContent`,
    /// `accessibilityFlags`, …); the parser strips the `android:` prefix, so
    /// the local names are what is matched here.
    pub fn from_document(document: &XmlDocument) -> Result<Self> {
        let root = &document.root;
        if root.name != "accessibility-service" {
            return Err(ServiceError::Internal(format!(
                "the accessibility meta-data root is <{}>, not <accessibility-service>",
                root.name
            )));
        }
        Ok(Self::from_node(root))
    }

    fn from_node(root: &XmlNode) -> Self {
        let mut config = Self::default();
        config.feedback_type = int_attr(root, "accessibilityFeedbackType");
        config.notification_timeout = i64::from(int_attr(root, "notificationTimeout"));
        config.event_types = int_attr(root, "accessibilityEventTypes");
        config.flags = int_attr(root, "accessibilityFlags");
        if bool_attr(root, "canRetrieveWindowContent") {
            config.capabilities |= CAPABILITY_CAN_RETRIEVE_WINDOW_CONTENT;
        }
        if bool_attr(root, "canPerformGestures") {
            config.capabilities |= CAPABILITY_CAN_PERFORM_GESTURES;
        }
        if bool_attr(root, "canTakeScreenshot") {
            config.capabilities |= CAPABILITY_CAN_TAKE_SCREENSHOT;
        }
        config
    }

    /// Resolve `@…` attribute values through the resource table (a config that
    /// points at an integer/boolean resource instead of a literal).
    fn resolve_references(&mut self, table: &ResourceTable, document: &XmlDocument) {
        let root = &document.root;
        for attribute in &root.attributes {
            let TypedValue::Reference(id) = attribute.value else {
                continue;
            };
            let Some(value) = resolve_value(table, id) else {
                continue;
            };
            let number = match value {
                ResValue::IntDec(n) => *n,
                ResValue::IntHex(n) => *n as i32,
                ResValue::Boolean(b) => i32::from(*b),
                _ => continue,
            };
            match attribute.name.as_str() {
                "accessibilityFlags" => self.flags = number,
                "accessibilityEventTypes" => self.event_types = number,
                "accessibilityFeedbackType" => self.feedback_type = number,
                "notificationTimeout" => self.notification_timeout = i64::from(number),
                "canRetrieveWindowContent" => {
                    self.capabilities &= !CAPABILITY_CAN_RETRIEVE_WINDOW_CONTENT;
                    if number != 0 {
                        self.capabilities |= CAPABILITY_CAN_RETRIEVE_WINDOW_CONTENT;
                    }
                }
                "canPerformGestures" => {
                    self.capabilities &= !CAPABILITY_CAN_PERFORM_GESTURES;
                    if number != 0 {
                        self.capabilities |= CAPABILITY_CAN_PERFORM_GESTURES;
                    }
                }
                "canTakeScreenshot" => {
                    self.capabilities &= !CAPABILITY_CAN_TAKE_SCREENSHOT;
                    if number != 0 {
                        self.capabilities |= CAPABILITY_CAN_TAKE_SCREENSHOT;
                    }
                }
                _ => {}
            }
        }
    }
}

/// Follow a resource id to the file path its entry names, through at most 16
/// references (`res/xml/…` for an `@xml/` resource).
fn resolve_file_path(table: &ResourceTable, id: u32) -> Option<String> {
    let mut id = id;
    for _ in 0..16 {
        match &table.resolve(id)?.value {
            ResValue::String(path) => return Some(path.clone()),
            ResValue::Reference(next) => id = *next,
            _ => return None,
        }
    }
    None
}

fn resolve_value(table: &ResourceTable, id: u32) -> Option<&ResValue> {
    let mut id = id;
    for _ in 0..16 {
        let value = &table.resolve(id)?.value;
        match value {
            ResValue::Reference(next) => id = *next,
            other => return Some(other),
        }
    }
    None
}

fn int_attr(root: &XmlNode, name: &str) -> i32 {
    match root.attribute(name).map(|a| &a.value) {
        Some(TypedValue::IntDec(n)) => *n,
        Some(TypedValue::IntHex(n)) => *n as i32,
        Some(TypedValue::Boolean(b)) => i32::from(*b),
        _ => 0,
    }
}

fn bool_attr(root: &XmlNode, name: &str) -> bool {
    matches!(
        root.attribute(name).map(|a| &a.value),
        Some(TypedValue::Boolean(true)) | Some(TypedValue::IntDec(1)) | Some(TypedValue::IntHex(1))
    )
}
