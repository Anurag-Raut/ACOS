//! Live `View` hierarchy extraction (T-40): walk the real framework `View`
//! tree of the running app with Java-level calls inside the VM.
//!
//! This is the honest counterpart of a screenshot: `WindowManagerGlobal`
//! holds the live `ViewRootImpl`s, each of which has an `mView` root. The
//! walker recurses over `ViewGroup.getChildAt` and reuses the framework's own
//! accessors (`getText`, `getContentDescription`, `getId`, `isShown`, ...).
//! Nothing is synthesized: a missing class/method, an empty root list or a
//! framework exception is returned as a structured [`TreeError`], never as an
//! empty-but-passing tree.
//!
//! The walker is deliberately conservative. Any optional accessor that is
//! absent (for example `isChecked` on a plain `View`) degrades to a default
//! instead of failing the whole tree; only the root discovery is fatal.
//!
//! The `ViewRootImpl` bounds are the same screen coordinates uiautomator
//! reports (`View.getLocationOnScreen`), so the tree can be compared with
//! `uiautomator dump` output by the parity tool.

use acos_vm::heap::{Handle, NULL};
use acos_vm::javabridge::JavaValue;
use acos_vm::Vm;

/// Node budget used when the caller does not specify one.
pub const DEFAULT_MAX_NODES: usize = 2000;

/// One node of the live `View` hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewNode {
    /// Tree-local identity (`n1`, `n2`, ...), stable for one snapshot.
    pub id: String,
    /// Fully qualified view class (`android.widget.TextView`).
    pub class: String,
    /// Android resource id, resolved through `Resources.getResourceName`.
    pub resource_id: String,
    /// `TextView.getText()` value, empty when absent.
    pub text: String,
    /// `View.getContentDescription()` value, empty when absent.
    pub content_desc: String,
    /// Screen bounds `[left, top, right, bottom]` in pixels, or all zeros when
    /// the framework could not report a location.
    pub bounds: [i32; 4],
    /// The part of `bounds` the user can see (clipped to the window), in screen
    /// pixels. `None` when the node is not visible to the user.
    pub visible_bounds: Option<[i32; 4]>,
    /// Whether the node is visible *to the user*
    /// ([`View.isVisibleToUser`](https://cs.android.com/android/platform/superproject/main/+/main:frameworks/base/core/java/android/view/View.java):
    /// attached with a visible window, it and all ancestors VISIBLE with alpha
    /// above zero, and its global visible rect not clipped away by an ancestor).
    pub visible: bool,
    /// `View.isEnabled()`.
    pub enabled: bool,
    /// `View.isFocused()`.
    pub focused: bool,
    /// `View.isClickable()`.
    pub clickable: bool,
    /// `canScrollVertically(1)` or `canScrollHorizontally(1)`.
    pub scrollable: bool,
    /// `isChecked()` when the view exposes it.
    pub checked: bool,
    /// Owning package (`View.getContext().getPackageName()`), when resolvable.
    pub package: String,
    /// Child views in layout order.
    pub children: Vec<ViewNode>,
}

/// The result of a live hierarchy walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewTree {
    /// Root windows in `WindowManagerGlobal` order.
    pub nodes: Vec<ViewNode>,
    /// Number of `ViewRootImpl` roots that contained a non-null `mView`.
    pub windows: usize,
    /// True when the node budget stopped the walk early.
    pub truncated: bool,
    /// The node budget that was applied.
    pub max_nodes: usize,
}

/// Why a live tree walk could not produce a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeError {
    /// The app has not added a window yet (no `ViewRootImpl`), or the process
    /// stopped before `Activity.onCreate` built a view. Never a pass.
    NotReady(String),
    /// The boot classpath does not expose the hierarchy API we need.
    Unsupported(String),
    /// A framework call failed unexpectedly.
    Internal(String),
}

impl TreeError {
    /// Stable ACOS error code for this failure.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            TreeError::NotReady(_) => "UNSUPPORTED_BACKEND",
            TreeError::Unsupported(_) => "UNSUPPORTED_BACKEND",
            TreeError::Internal(_) => "INTERNAL",
        }
    }

    /// Machine-readable reason detail.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            TreeError::NotReady(_) => "not_ready",
            TreeError::Unsupported(_) => "no_view_hierarchy_api",
            TreeError::Internal(_) => "view_walk_failed",
        }
    }

    /// Human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            TreeError::NotReady(m) | TreeError::Unsupported(m) | TreeError::Internal(m) => m,
        }
    }
}

/// Walk the live `View` hierarchy of `vm` and append the platform overlays the
/// system service is currently showing (today: a toast the app asked for).
///
/// `android.widget.Toast` is **not** an app window: on device
/// `NotificationManagerService` renders it from the system process as a
/// `TYPE_TOAST` window with `GRAVITY_BOTTOM|CENTER_HORIZONTAL`. ACOS runs the
/// service side in the same process, so the runtime plays that system-UI role
/// and reports the visible toast as a `ViewNode` with the **real** text the
/// framework serialised into `INotificationManager.enqueueTextToast`. Nothing
/// here invents text or geometry: the text comes from the request, the frame
/// from the standard toast gravity for the reference display.
pub fn append_toasts(
    mut tree: ViewTree,
    toasts: &[acos_services::declared::ToastRecord],
) -> ViewTree {
    if toasts.is_empty() {
        return tree;
    }
    let (width, height, density) = (
        acos_wm::display::DISPLAY_WIDTH,
        acos_wm::display::DISPLAY_HEIGHT,
        acos_wm::display::DENSITY_DPI as i32,
    );
    // 64dp above the navigation bar, 48dp tall: the platform toast window for
    // `GRAVITY_BOTTOM|CENTER_HORIZONTAL` on this display (px = dp * dpi / 160).
    let px = |dp: i32| dp * density / 160;
    let toast_height = px(48);
    let margin = px(64) + acos_wm::display::NAV_BAR_BOTTOM;
    let top = height - margin - toast_height;
    let bottom = top + toast_height;
    for toast in toasts {
        tree.nodes.push(ViewNode {
            id: format!("toast{}", tree.nodes.len() + 1),
            class: "android.widget.Toast".to_string(),
            resource_id: String::new(),
            text: toast.text.clone(),
            content_desc: String::new(),
            bounds: [0, top, width, bottom],
            visible_bounds: Some([0, top, width, bottom]),
            visible: true,
            enabled: true,
            focused: false,
            clickable: false,
            scrollable: false,
            checked: false,
            package: toast.package.clone(),
            children: Vec::new(),
        });
    }
    tree
}

/// The toasts the system service currently has on screen, oldest first.
#[must_use]
pub fn visible_toasts(host: &acos_services::ServiceHost) -> Vec<acos_services::declared::ToastRecord> {
    let Some(state) = host.notification_state() else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now();
    let st = state.lock().unwrap();
    st.shown_toasts
        .iter()
        .filter(|t| t.is_visible_at(now))
        .cloned()
        .collect()
}

/// Walk the live `View` hierarchy of `vm`.
///
/// Returns [`TreeError::NotReady`] when there is no root window, which is the
/// honest answer before the app reaches a view hierarchy.
pub fn walk(vm: &mut Vm, max_nodes: usize) -> Result<ViewTree, TreeError> {
    let max_nodes = if max_nodes == 0 {
        DEFAULT_MAX_NODES
    } else {
        max_nodes
    };

    let global = match vm.call_static(
        "android.view.WindowManagerGlobal",
        "getInstance",
        "()Landroid/view/WindowManagerGlobal;",
        &[],
    ) {
        Ok(JavaValue::Object(h)) if h != NULL => h,
        Ok(_) => {
            return Err(TreeError::Unsupported(
                "android.view.WindowManagerGlobal.getInstance() returned null".to_string(),
            ))
        }
        Err(e) => {
            vm.pending_exception = NULL;
            return Err(TreeError::Unsupported(format!(
                "cannot reach android.view.WindowManagerGlobal.getInstance(): {e}"
            )));
        }
    };

    // Prefer `ViewRootImpl` roots; fall back to `WindowManagerGlobal.mViews`
    // (the same list, holding the root `View`s directly) when the accessor is
    // not present in the extracted boot classpath. Both are Java-level reads
    // of the real hierarchy.
    let roots = match vm.call_method(global, "getRootViews", "()Ljava/util/ArrayList;", &[]) {
        Ok(JavaValue::Object(h)) if h != NULL => Some(h),
        Ok(_) => None,
        Err(_) => {
            vm.pending_exception = NULL;
            None
        }
    };
    let roots = match roots {
        Some(h) => h,
        None => match vm.get_field(global, "mViews") {
            Ok(JavaValue::Object(h)) if h != NULL => h,
            _ => {
                vm.pending_exception = NULL;
                return Err(TreeError::Unsupported(
                    "WindowManagerGlobal exposes neither getRootViews() nor mViews; \
                     cannot reach the live View hierarchy"
                        .to_string(),
                ));
            }
        },
    };

    let count = match int_call(vm, roots, "size", "()I") {
        Some(n) if n > 0 => n as usize,
        _ => {
            return Err(TreeError::NotReady(
                "WindowManagerGlobal has no root view: the app has not added a window yet"
                    .to_string(),
            ))
        }
    };

    let mut walker = Walker {
        vm,
        budget: max_nodes,
        emitted: 0,
        truncated: false,
    };
    let mut nodes = Vec::new();
    let mut windows = 0usize;
    for i in 0..count {
        let Some(entry) = object_call(
            walker.vm,
            roots,
            "get",
            "(I)Ljava/lang/Object;",
            &[JavaValue::Int(i as i32)],
        ) else {
            continue;
        };
        // A `ViewRootImpl` entry has `mView`; a plain root `View` does not, so
        // use the entry itself.
        let view = match walker.vm.get_field(entry, "mView") {
            Ok(JavaValue::Object(h)) if h != NULL => h,
            Ok(_) => continue,
            Err(_) => {
                walker.vm.pending_exception = NULL;
                entry
            }
        };
        windows += 1;
        let root_bounds = root_bounds(walker.vm, view);
        if let Some(node) = walker.node(view, 0, 0, root_bounds) {
            nodes.push(node);
        }
        if walker.emitted >= walker.budget {
            break;
        }
    }
    walker.vm.pending_exception = NULL;

    if windows == 0 {
        return Err(TreeError::NotReady(
            "the root list exists but holds no non-null root view".to_string(),
        ));
    }

    Ok(ViewTree {
        nodes,
        windows,
        truncated: walker.truncated,
        max_nodes,
    })
}

struct Walker<'a> {
    vm: &'a mut Vm,
    budget: usize,
    emitted: usize,
    truncated: bool,
}

impl Walker<'_> {
    fn node(
        &mut self,
        view: Handle,
        parent_x: i32,
        parent_y: i32,
        root_bounds: [i32; 4],
    ) -> Option<ViewNode> {
        if self.emitted >= self.budget {
            self.truncated = true;
            return None;
        }
        self.emitted += 1;
        let id = format!("n{}", self.emitted);

        let class = self
            .vm
            .class_of_object(view)
            .map(|c| self.vm.class_desc_of_id(c))
            .unwrap_or_default();

        let mut text = string_call(self.vm, view, "getText", "()Ljava/lang/CharSequence;", &[])
            .unwrap_or_default();
        if text.is_empty() {
            // uiautomator reports a TextView/EditText `hint` as its text when the
            // field is empty (the ART reference trees do the same), so an empty
            // input is observable by its hint instead of looking like a blank
            // node. A typed value always wins.
            text = string_call(self.vm, view, "getHint", "()Ljava/lang/CharSequence;", &[])
                .unwrap_or_default();
        }
        let content_desc = string_call(
            self.vm,
            view,
            "getContentDescription",
            "()Ljava/lang/CharSequence;",
            &[],
        )
        .unwrap_or_default();
        let resource_id = resource_id(self.vm, view);
        let package = package_name(self.vm, view);

        let left = int_call(self.vm, view, "getLeft", "()I").unwrap_or(0);
        let top = int_call(self.vm, view, "getTop", "()I").unwrap_or(0);
        let width = int_call(self.vm, view, "getWidth", "()I").unwrap_or(0);
        let height = int_call(self.vm, view, "getHeight", "()I").unwrap_or(0);
        let (sx, sy) = location_on_screen(self.vm, view)
            .unwrap_or((parent_x.saturating_add(left), parent_y.saturating_add(top)));
        let bounds = [sx, sy, sx.saturating_add(width), sy.saturating_add(height)];

        // `isVisibleToUser()` is the framework's own answer to "can the user see
        // this view" (attachment, window visibility, ancestor visibility, alpha,
        // and ancestor clipping via `getGlobalVisibleRect`). `isShown()` alone
        // ignores clipping and is what let off-screen pager pages report
        // `visible: true` (GR2 W4). Fall back to `isShown()` only if the
        // framework method is absent from this boot classpath.
        let visible = bool_call(self.vm, view, "isVisibleToUser", "()Z", &[])
            .or_else(|| bool_call(self.vm, view, "isShown", "()Z", &[]))
            .unwrap_or(false);
        let visible_bounds = visible_bounds(bounds, root_bounds, visible);
        let enabled = bool_call(self.vm, view, "isEnabled", "()Z", &[]).unwrap_or(true);
        let focused = bool_call(self.vm, view, "isFocused", "()Z", &[]).unwrap_or(false);
        let clickable = bool_call(self.vm, view, "isClickable", "()Z", &[]).unwrap_or(false);
        let scrollable = bool_call(
            self.vm,
            view,
            "canScrollVertically",
            "(I)Z",
            &[JavaValue::Int(1)],
        )
        .or_else(|| {
            bool_call(
                self.vm,
                view,
                "canScrollHorizontally",
                "(I)Z",
                &[JavaValue::Int(1)],
            )
        })
        .unwrap_or(false);
        let checked = bool_call(self.vm, view, "isChecked", "()Z", &[]).unwrap_or(false);

        let mut children = Vec::new();
        if let Some(child_count) = int_call(self.vm, view, "getChildCount", "()I") {
            for i in 0..child_count.max(0) {
                if self.emitted >= self.budget {
                    self.truncated = true;
                    break;
                }
                let Some(child) = object_call(
                    self.vm,
                    view,
                    "getChildAt",
                    "(I)Landroid/view/View;",
                    &[JavaValue::Int(i)],
                ) else {
                    continue;
                };
                if let Some(node) = self.node(child, sx, sy, root_bounds) {
                    children.push(node);
                }
            }
        }

        Some(ViewNode {
            id,
            class,
            resource_id,
            text,
            content_desc,
            bounds,
            visible_bounds,
            visible,
            enabled,
            focused,
            clickable,
            scrollable,
            checked,
            package,
            children,
        })
    }
}

/// The root view's own rect on screen, the window rect a node is clipped to.
fn root_bounds(vm: &mut Vm, root: Handle) -> [i32; 4] {
    let left = int_call(vm, root, "getLeft", "()I").unwrap_or(0);
    let top = int_call(vm, root, "getTop", "()I").unwrap_or(0);
    let width = int_call(vm, root, "getWidth", "()I").unwrap_or(0);
    let height = int_call(vm, root, "getHeight", "()I").unwrap_or(0);
    let (sx, sy) = location_on_screen(vm, root).unwrap_or((left, top));
    [sx, sy, sx.saturating_add(width), sy.saturating_add(height)]
}

/// The user-visible rect of `bounds` clipped to the window rect, or `None` when
/// the node is not visible to the user or nothing of it falls inside the window.
///
/// The *flag* is the framework's (`isVisibleToUser`, which also accounts for
/// ancestor scroll/clip); this rect is the window/display clip. When the window
/// rect is unknown (all zeros) the node's own bounds are the visible rect.
fn visible_bounds(bounds: [i32; 4], root: [i32; 4], visible: bool) -> Option<[i32; 4]> {
    if !visible {
        return None;
    }
    if root[2] <= root[0] || root[3] <= root[1] {
        return Some(bounds);
    }
    let clipped = [
        bounds[0].max(root[0]),
        bounds[1].max(root[1]),
        bounds[2].min(root[2]),
        bounds[3].min(root[3]),
    ];
    (clipped[2] > clipped[0] && clipped[3] > clipped[1]).then_some(clipped)
}

/// Call `obj.name(sig)` and keep the object result; any failure is `None` and
/// clears a pending framework exception so the walk can continue.
fn object_call(
    vm: &mut Vm,
    obj: Handle,
    name: &str,
    sig: &str,
    args: &[JavaValue],
) -> Option<Handle> {
    match vm.call_method(obj, name, sig, args) {
        Ok(JavaValue::Object(h)) if h != NULL => Some(h),
        Ok(_) => None,
        Err(_) => {
            vm.pending_exception = NULL;
            None
        }
    }
}

fn int_call(vm: &mut Vm, obj: Handle, name: &str, sig: &str) -> Option<i32> {
    match vm.call_method(obj, name, sig, &[]) {
        Ok(JavaValue::Int(v)) => Some(v),
        Ok(_) => None,
        Err(_) => {
            vm.pending_exception = NULL;
            None
        }
    }
}

fn bool_call(
    vm: &mut Vm,
    obj: Handle,
    name: &str,
    sig: &str,
    args: &[JavaValue],
) -> Option<bool> {
    match vm.call_method(obj, name, sig, args) {
        Ok(JavaValue::Boolean(v)) => Some(v),
        Ok(_) => None,
        Err(_) => {
            vm.pending_exception = NULL;
            None
        }
    }
}

/// Read a `CharSequence`-returning accessor and stringify it through the
/// framework's own `toString()` (handles `SpannableString`, not just `String`).
fn string_call(
    vm: &mut Vm,
    obj: Handle,
    name: &str,
    sig: &str,
    args: &[JavaValue],
) -> Option<String> {
    let cs = object_call(vm, obj, name, sig, args)?;
    let s = object_call(vm, cs, "toString", "()Ljava/lang/String;", &[])?;
    vm.string_to_rust(s)
}

fn resource_id(vm: &mut Vm, view: Handle) -> String {
    let Some(id) = int_call(vm, view, "getId", "()I") else {
        return String::new();
    };
    if id == 0 {
        return String::new();
    }
    if let Some(res) = object_call(
        vm,
        view,
        "getResources",
        "()Landroid/content/res/Resources;",
        &[],
    ) {
        if let Some(name) = string_call(vm, res, "getResourceName", "(I)Ljava/lang/String;", &[JavaValue::Int(id)])
        {
            return name;
        }
    }
    format!("@0x{id:08x}")
}

fn package_name(vm: &mut Vm, view: Handle) -> String {
    let Some(context) = object_call(vm, view, "getContext", "()Landroid/content/Context;", &[])
    else {
        return String::new();
    };
    string_call(vm, context, "getPackageName", "()Ljava/lang/String;", &[]).unwrap_or_default()
}

/// Screen location via the framework's own `getLocationOnScreen(int[])`.
fn location_on_screen(vm: &mut Vm, view: Handle) -> Option<(i32, i32)> {
    let array = vm
        .new_array("I", &[JavaValue::Int(0), JavaValue::Int(0)])
        .ok()?;
    match vm.call_method(
        view,
        "getLocationOnScreen",
        "([I)V",
        &[JavaValue::Object(array)],
    ) {
        Ok(_) => {}
        Err(_) => {
            vm.pending_exception = NULL;
            return None;
        }
    }
    let x = match vm.get_array_element(array, 0) {
        Ok(JavaValue::Int(v)) => v,
        _ => return None,
    };
    let y = match vm.get_array_element(array, 1) {
        Ok(JavaValue::Int(v)) => v,
        _ => return None,
    };
    Some((x, y))
}

/// Depth-first walk of a [`ViewTree`], including every node.
pub fn all_nodes<'a>(nodes: &'a [ViewNode], out: &mut Vec<&'a ViewNode>) {
    for node in nodes {
        out.push(node);
        all_nodes(&node.children, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_coded_not_passes() {
        let not_ready = TreeError::NotReady("no window".to_string());
        assert_eq!(not_ready.code(), "UNSUPPORTED_BACKEND");
        assert_eq!(not_ready.reason(), "not_ready");
        let unsupported = TreeError::Unsupported("no api".to_string());
        assert_eq!(unsupported.reason(), "no_view_hierarchy_api");
        let internal = TreeError::Internal("boom".to_string());
        assert_eq!(internal.code(), "INTERNAL");
    }
}
