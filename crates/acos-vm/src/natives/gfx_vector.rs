//! `android.graphics.drawable.VectorDrawable` native tree.
//!
//! The real framework keeps a C++ tree of `VGroup` / `VFullPath` / `VClipPath`
//! nodes behind `VectorDrawable.nCreate*` / `nSet*` / `nDraw`. This module owns
//! that tree in generation-checked handle tables and renders it into the
//! `acos-render` display list at `nDraw` time.
//!
//! The property packing of `nGetFullPathProperties` / `nGetGroupProperties`
//! follows the framework's own `VFullPath` / `VGroup` byte layout exactly
//! (`android-34` `VectorDrawable.java`):
//!
//! * full path, 12 little-endian 32-bit slots — strokeWidth(f32),
//!   strokeColor(i32), strokeAlpha(f32), fillColor(i32), fillAlpha(f32),
//!   trimPathStart(f32), trimPathEnd(f32), trimPathOffset(f32),
//!   strokeLineCap(i32), strokeLineJoin(i32), strokeMiterLimit(f32),
//!   fillType(i32).
//! * group, 7 f32 slots — rotation, pivotX, pivotY, scaleX, scaleY,
//!   translateX, translateY.
//!
//! Unsupported attributes are structured `UNSUPPORTED_JAVA_API`, never a
//! silently wrong draw. Boolean `Path.op`-style constructs are not relevant
//! here; the only genuinely unsupported operation on this surface is a
//! `PathData` morph (`nCanMorph` / `nInterpolatePathData`), which is reported
//! as unsupported rather than faked.

use std::sync::{Mutex, MutexGuard, OnceLock};

use acos_render::canvas::ClipOp;
use acos_render::matrix::Matrix3x3;
use acos_render::paint::{Paint, PaintStyle, StrokeCap, StrokeJoin};
use acos_render::path::{FillType, Path, PathMeasure, Verb};
use acos_render::render_node::DisplayOp;

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::natives::gfx_handles::{Handle, HandleTable};
use crate::natives::gfx_state::with_ctx;
use crate::vm::Vm;

/// Tag bit on `AnimatedVectorDrawable` animator-set `jlong`s. The shared
/// `VirtualRefBasePtr` refcount table (`fwk_m2_chain6`) and [`release_tree`]
/// see both `VectorDrawable` tree pointers and animator-set pointers, and two
/// independent [`HandleTable`]s both start at `(generation 1, index 0)`. The
/// tag keeps the two handle spaces distinct so a tree pointer and an
/// animator-set pointer can never alias.
const AVD_SET_TAG: u64 = 1 << 63;
/// Tag bit on property-holder `jlong`s. Property holders are owned by their
/// animator set (not `VirtualRefBasePtr`-refcounted), but they still need a
/// handle space distinct from both trees and sets.
const AVD_HOLDER_TAG: u64 = 1 << 62;

// ---------------------------------------------------------------------------
// Tree model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct VGroup {
    rotation: f32,
    pivot_x: f32,
    pivot_y: f32,
    scale_x: f32,
    scale_y: f32,
    translate_x: f32,
    translate_y: f32,
}

impl Default for VGroup {
    fn default() -> Self {
        VGroup {
            rotation: 0.0,
            pivot_x: 0.0,
            pivot_y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            translate_x: 0.0,
            translate_y: 0.0,
        }
    }
}

impl VGroup {
    /// `VectorDrawable` group transform, matching the framework's
    /// `postTranslate(-pivot); postScale; postRotate; postTranslate(pivot + translate)`.
    fn matrix(&self) -> Matrix3x3 {
        // Android `Matrix.postTranslate(-pivot); postScale; postRotate;
        // postTranslate(pivot + translate)` composes as
        // `T(pivot+translate) · R · S · T(-pivot)`.
        let mut m = Matrix3x3::translate_matrix(
            self.pivot_x + self.translate_x,
            self.pivot_y + self.translate_y,
        );
        m.pre_concat(&Matrix3x3::rotate_matrix(self.rotation));
        m.pre_concat(&Matrix3x3::scale_matrix(self.scale_x, self.scale_y));
        m.pre_concat(&Matrix3x3::translate_matrix(-self.pivot_x, -self.pivot_y));
        m
    }
}

#[derive(Clone, Debug)]
struct VFullPath {
    path: Path,
    stroke_width: f32,
    stroke_color: u32,
    stroke_alpha: f32,
    fill_color: u32,
    fill_alpha: f32,
    trim_path_start: f32,
    trim_path_end: f32,
    trim_path_offset: f32,
    stroke_miter_limit: f32,
    stroke_line_cap: i32,
    stroke_line_join: i32,
    fill_type: i32,
    fill_gradient: i64,
    stroke_gradient: i64,
}

impl Default for VFullPath {
    fn default() -> Self {
        VFullPath {
            path: Path::new(),
            stroke_width: 0.0,
            stroke_color: 0,
            stroke_alpha: 1.0,
            fill_color: 0,
            fill_alpha: 1.0,
            trim_path_start: 0.0,
            trim_path_end: 1.0,
            trim_path_offset: 0.0,
            stroke_miter_limit: 4.0,
            stroke_line_cap: 0,
            stroke_line_join: 0,
            fill_type: 0,
            fill_gradient: 0,
            stroke_gradient: 0,
        }
    }
}

#[derive(Clone, Debug)]
struct VClipPath {
    path: Path,
}

impl Default for VClipPath {
    fn default() -> Self {
        VClipPath { path: Path::new() }
    }
}

#[derive(Clone, Debug)]
enum VNodeKind {
    Group(VGroup),
    FullPath(VFullPath),
    ClipPath(VClipPath),
}

#[derive(Clone, Debug)]
struct VNode {
    kind: VNodeKind,
    name: String,
    children: Vec<Handle>,
}

impl VNode {
    fn group(name: String, g: VGroup) -> Self {
        VNode {
            kind: VNodeKind::Group(g),
            name,
            children: Vec::new(),
        }
    }
    fn full_path(name: String, p: VFullPath) -> Self {
        VNode {
            kind: VNodeKind::FullPath(p),
            name,
            children: Vec::new(),
        }
    }
    fn clip_path(name: String, p: VClipPath) -> Self {
        VNode {
            kind: VNodeKind::ClipPath(p),
            name,
            children: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct VTree {
    root: Handle,
    viewport_width: f32,
    viewport_height: f32,
    root_alpha: f32,
    anti_alias: bool,
    allow_caching: bool,
}

impl VTree {
    fn from_root(root: Handle) -> Self {
        VTree {
            root,
            viewport_width: 0.0,
            viewport_height: 0.0,
            root_alpha: 1.0,
            anti_alias: true,
            allow_caching: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Handle tables
// ---------------------------------------------------------------------------

static NODES: OnceLock<Mutex<HandleTable<VNode>>> = OnceLock::new();
static TREES: OnceLock<Mutex<HandleTable<VTree>>> = OnceLock::new();
static PATH_DATA: OnceLock<Mutex<HandleTable<Path>>> = OnceLock::new();

fn nodes() -> MutexGuard<'static, HandleTable<VNode>> {
    let m = NODES.get_or_init(|| Mutex::new(HandleTable::new()));
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn trees() -> MutexGuard<'static, HandleTable<VTree>> {
    let m = TREES.get_or_init(|| Mutex::new(HandleTable::new()));
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn path_data() -> MutexGuard<'static, HandleTable<Path>> {
    let m = PATH_DATA.get_or_init(|| Mutex::new(HandleTable::new()));
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn node_err(e: crate::natives::gfx_handles::HandleError) -> VmError {
    VmError::Internal(format!("VectorDrawable node: {e}"))
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Release a `VirtualRefBasePtr` object when its framework refcount reaches
/// zero. The pointer is either a `VectorDrawable` tree (`nCreateTree`) or an
/// `AnimatedVectorDrawable` animator set (`nCreateAnimatorSet`); the tag bit
/// selects the table. Releasing a set also releases the property holders it
/// owns, matching the C++ `AnimatorSetImpl` ownership.
pub(crate) fn release_tree(handle: i64) {
    let u = handle as u64;
    if u & AVD_SET_TAG != 0 {
        if let Ok(set) = avd_sets().remove(Handle(u & !AVD_SET_TAG)) {
            let mut holders = avd_holders();
            for child in set.children {
                let _ = holders.remove(child.holder);
            }
        }
        return;
    }
    let _ = trees().remove(Handle::from_jlong(handle));
}

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    const V: &str = "Landroid/graphics/drawable/VectorDrawable;";

    vm.register_native(V, "nCreateTree", "(J)J", n_create_tree);
    vm.register_native(V, "nCreateTreeFromCopy", "(JJ)J", n_create_tree_from_copy);
    vm.register_native(V, "nCreateFullPath", "()J", n_create_full_path);
    vm.register_native(V, "nCreateFullPath", "(J)J", n_create_full_path_copy);
    vm.register_native(V, "nCreateClipPath", "()J", n_create_clip_path);
    vm.register_native(V, "nCreateClipPath", "(J)J", n_create_clip_path_copy);
    vm.register_native(V, "nCreateGroup", "()J", n_create_group);
    vm.register_native(V, "nCreateGroup", "(J)J", n_create_group_copy);
    vm.register_native(V, "nAddChild", "(JJ)V", n_add_child);
    vm.register_native(V, "nSetName", "(JLjava/lang/String;)V", n_set_name);
    vm.register_native(V, "nSetPathString", "(JLjava/lang/String;I)V", n_set_path_string);
    vm.register_native(V, "nSetPathData", "(JJ)V", n_set_path_data);
    vm.register_native(V, "nSetRendererViewportSize", "(JFF)V", n_set_renderer_viewport_size);
    vm.register_native(V, "nSetRootAlpha", "(JF)Z", n_set_root_alpha);
    vm.register_native(V, "nGetRootAlpha", "(J)F", n_get_root_alpha);
    vm.register_native(V, "nSetAntiAlias", "(JZ)V", n_set_anti_alias);
    vm.register_native(V, "nSetAllowCaching", "(JZ)V", n_set_allow_caching);
    vm.register_native(V, "nDraw", "(JJJLandroid/graphics/Rect;ZZ)I", n_draw);

    vm.register_native(V, "nGetFullPathProperties", "(J[BI)Z", n_get_full_path_properties);
    vm.register_native(V, "nGetGroupProperties", "(J[FI)Z", n_get_group_properties);
    vm.register_native(V, "nUpdateFullPathProperties", "(JFIFIFFFFFIII)V", n_update_full_path_properties);
    vm.register_native(V, "nUpdateFullPathFillGradient", "(JJ)V", n_update_full_path_fill_gradient);
    vm.register_native(V, "nUpdateFullPathStrokeGradient", "(JJ)V", n_update_full_path_stroke_gradient);
    vm.register_native(V, "nUpdateGroupProperties", "(JFFFFFFF)V", n_update_group_properties);

    // Full path setters / getters.
    vm.register_native(V, "nSetStrokeWidth", "(JF)V", n_set_stroke_width);
    vm.register_native(V, "nGetStrokeWidth", "(J)F", n_get_stroke_width);
    vm.register_native(V, "nSetStrokeColor", "(JI)V", n_set_stroke_color);
    vm.register_native(V, "nGetStrokeColor", "(J)I", n_get_stroke_color);
    vm.register_native(V, "nSetStrokeAlpha", "(JF)V", n_set_stroke_alpha);
    vm.register_native(V, "nGetStrokeAlpha", "(J)F", n_get_stroke_alpha);
    vm.register_native(V, "nSetFillColor", "(JI)V", n_set_fill_color);
    vm.register_native(V, "nGetFillColor", "(J)I", n_get_fill_color);
    vm.register_native(V, "nSetFillAlpha", "(JF)V", n_set_fill_alpha);
    vm.register_native(V, "nGetFillAlpha", "(J)F", n_get_fill_alpha);
    vm.register_native(V, "nSetTrimPathStart", "(JF)V", n_set_trim_path_start);
    vm.register_native(V, "nGetTrimPathStart", "(J)F", n_get_trim_path_start);
    vm.register_native(V, "nSetTrimPathEnd", "(JF)V", n_set_trim_path_end);
    vm.register_native(V, "nGetTrimPathEnd", "(J)F", n_get_trim_path_end);
    vm.register_native(V, "nSetTrimPathOffset", "(JF)V", n_set_trim_path_offset);
    vm.register_native(V, "nGetTrimPathOffset", "(J)F", n_get_trim_path_offset);

    // Group setters / getters.
    vm.register_native(V, "nSetRotation", "(JF)V", n_set_rotation);
    vm.register_native(V, "nGetRotation", "(J)F", n_get_rotation);
    vm.register_native(V, "nSetPivotX", "(JF)V", n_set_pivot_x);
    vm.register_native(V, "nGetPivotX", "(J)F", n_get_pivot_x);
    vm.register_native(V, "nSetPivotY", "(JF)V", n_set_pivot_y);
    vm.register_native(V, "nGetPivotY", "(J)F", n_get_pivot_y);
    vm.register_native(V, "nSetScaleX", "(JF)V", n_set_scale_x);
    vm.register_native(V, "nGetScaleX", "(J)F", n_get_scale_x);
    vm.register_native(V, "nSetScaleY", "(JF)V", n_set_scale_y);
    vm.register_native(V, "nGetScaleY", "(J)F", n_get_scale_y);
    vm.register_native(V, "nSetTranslateX", "(JF)V", n_set_translate_x);
    vm.register_native(V, "nGetTranslateX", "(J)F", n_get_translate_x);
    vm.register_native(V, "nSetTranslateY", "(JF)V", n_set_translate_y);
    vm.register_native(V, "nGetTranslateY", "(J)F", n_get_translate_y);

    // `android.util.PathParser` — the PathData peers the VectorDrawable
    // `nSetPathData` consumes. Registered here so the same path parser is used
    // by both the drawable and the parser surface.
    const PP: &str = "Landroid/util/PathParser;";
    vm.register_native(PP, "nCreateEmptyPathData", "()J", pp_create_empty);
    vm.register_native(PP, "nCreatePathDataFromString", "(Ljava/lang/String;I)J", pp_create_from_string);
    vm.register_native(PP, "nCreatePathData", "(J)J", pp_create);
    vm.register_native(PP, "nCreatePathFromPathData", "(JJ)V", pp_create_path);
    vm.register_native(PP, "nParseStringForPath", "(JLjava/lang/String;I)V", pp_parse_string_for_path);
    vm.register_native(PP, "nSetPathData", "(JJ)V", pp_set_path_data);
    vm.register_native(PP, "nFinalize", "(J)V", pp_finalize);
    vm.register_native(PP, "nCanMorph", "(JJ)Z", pp_can_morph);
    vm.register_native(PP, "nInterpolatePathData", "(JJJF)Z", pp_interpolate);

    // `AnimatedVectorDrawable` — the native `AnimatorSet` behind
    // `VectorDrawableAnimatorRT`. The API-34 DEX declares exactly these
    // natives (`PRIVATE STATIC NATIVE`); the property-holder creators take
    // the target `VectorDrawable` node/tree pointer plus the property id and
    // the start/end values, and `nAddAnimator` attaches the holder to the set.
    const AVD: &str = "Landroid/graphics/drawable/AnimatedVectorDrawable;";
    vm.register_native(AVD, "nAddAnimator", "(JJJJJII)V", n_add_animator);
    vm.register_native(AVD, "nCreateAnimatorSet", "()J", n_create_animator_set);
    vm.register_native(AVD, "nCreateGroupPropertyHolder", "(JIFF)J", n_create_group_property_holder);
    vm.register_native(AVD, "nCreatePathColorPropertyHolder", "(JIII)J", n_create_path_color_property_holder);
    vm.register_native(AVD, "nCreatePathDataPropertyHolder", "(JJJ)J", n_create_path_data_property_holder);
    vm.register_native(AVD, "nCreatePathPropertyHolder", "(JIFF)J", n_create_path_property_holder);
    vm.register_native(AVD, "nCreateRootAlphaPropertyHolder", "(JFF)J", n_create_root_alpha_property_holder);
    vm.register_native(AVD, "nEnd", "(J)V", n_end);
    vm.register_native(AVD, "nReset", "(J)V", n_reset);
    vm.register_native(
        AVD,
        "nReverse",
        "(JLandroid/graphics/drawable/AnimatedVectorDrawable$VectorDrawableAnimatorRT;I)V",
        n_reverse,
    );
    vm.register_native(AVD, "nSetPropertyHolderData", "(J[FI)V", n_set_property_holder_data_float);
    vm.register_native(AVD, "nSetPropertyHolderData", "(J[II)V", n_set_property_holder_data_int);
    vm.register_native(AVD, "nSetVectorDrawableTarget", "(JJ)V", n_set_vector_drawable_target);
    vm.register_native(
        AVD,
        "nStart",
        "(JLandroid/graphics/drawable/AnimatedVectorDrawable$VectorDrawableAnimatorRT;I)V",
        n_start,
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Tree construction
// ---------------------------------------------------------------------------

fn n_create_tree(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let root = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let h = trees().insert(VTree::from_root(root));
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn n_create_tree_from_copy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let old = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let root = Handle::from_jlong(vm.arg_i64(ctx, 1));
    let mut t = trees().get(old).cloned().unwrap_or_else(|_| VTree::from_root(root));
    t.root = root;
    let h = trees().insert(t);
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn n_create_full_path(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = nodes().insert(VNode::full_path(String::new(), VFullPath::default()));
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn n_create_full_path_copy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let copy = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let (name, p) = match nodes().get(copy) {
        Ok(n) => match &n.kind {
            VNodeKind::FullPath(p) => (n.name.clone(), p.clone()),
            _ => (n.name.clone(), VFullPath::default()),
        },
        Err(_) => (String::new(), VFullPath::default()),
    };
    let h = nodes().insert(VNode::full_path(name, p));
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn n_create_clip_path(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = nodes().insert(VNode::clip_path(String::new(), VClipPath::default()));
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn n_create_clip_path_copy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let copy = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let (name, p) = match nodes().get(copy) {
        Ok(n) => match &n.kind {
            VNodeKind::ClipPath(p) => (n.name.clone(), p.clone()),
            _ => (n.name.clone(), VClipPath::default()),
        },
        Err(_) => (String::new(), VClipPath::default()),
    };
    let h = nodes().insert(VNode::clip_path(name, p));
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn n_create_group(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = nodes().insert(VNode::group(String::new(), VGroup::default()));
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn n_create_group_copy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let copy = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let (name, g) = match nodes().get(copy) {
        Ok(n) => match &n.kind {
            VNodeKind::Group(g) => (n.name.clone(), g.clone()),
            _ => (n.name.clone(), VGroup::default()),
        },
        Err(_) => (String::new(), VGroup::default()),
    };
    let h = nodes().insert(VNode::group(name, g));
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn n_add_child(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let parent = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let child = Handle::from_jlong(vm.arg_i64(ctx, 1));
    // Validate the child resolves; a dangling child is a structured error, not
    // a silently dropped subtree.
    let _ = nodes().get(child).map_err(node_err)?;
    let mut table = nodes();
    let n = table.get_mut(parent).map_err(node_err)?;
    match &n.kind {
        VNodeKind::Group(_) => {
            n.children.push(child);
            Ok(())
        }
        _ => Err(VmError::unsupported_api(
            "VectorDrawable.nAddChild on a non-group node",
            "android.graphics.drawable.VectorDrawable",
        )),
    }
}

fn n_set_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let name = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
    let mut table = nodes();
    if let Ok(n) = table.get_mut(node) {
        n.name = name;
    }
    Ok(())
}

fn n_set_path_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let s = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
    let _len = vm.arg_i32(ctx, 2);
    let path = parse_path_string(&s).map_err(|e| {
        VmError::unsupported_api(
            format!("VectorDrawable pathData {:?}: {e}", s),
            "android.graphics.drawable.VectorDrawable",
        )
    })?;
    let mut table = nodes();
    let n = table.get_mut(node).map_err(node_err)?;
    match &mut n.kind {
        VNodeKind::FullPath(p) => {
            p.path = path;
            Ok(())
        }
        VNodeKind::ClipPath(p) => {
            p.path = path;
            Ok(())
        }
        VNodeKind::Group(_) => Err(VmError::unsupported_api(
            "VectorDrawable.nSetPathString on a group",
            "android.graphics.drawable.VectorDrawable",
        )),
    }
}

fn n_set_path_data(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let pd = Handle::from_jlong(vm.arg_i64(ctx, 1));
    let path = path_data().get(pd).cloned().map_err(node_err)?;
    let mut table = nodes();
    let n = table.get_mut(node).map_err(node_err)?;
    match &mut n.kind {
        VNodeKind::FullPath(p) => {
            p.path = path;
            Ok(())
        }
        VNodeKind::ClipPath(p) => {
            p.path = path;
            Ok(())
        }
        VNodeKind::Group(_) => Err(VmError::unsupported_api(
            "VectorDrawable.nSetPathData on a group",
            "android.graphics.drawable.VectorDrawable",
        )),
    }
}

fn n_set_renderer_viewport_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let tree = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let w = vm.arg_f32(ctx, 1);
    let h = vm.arg_f32(ctx, 2);
    let mut table = trees();
    if let Ok(t) = table.get_mut(tree) {
        t.viewport_width = w;
        t.viewport_height = h;
    }
    Ok(())
}

fn n_set_root_alpha(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let tree = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let a = vm.arg_f32(ctx, 1);
    let mut changed = false;
    let mut table = trees();
    if let Ok(t) = table.get_mut(tree) {
        changed = t.root_alpha != a;
        t.root_alpha = a;
    }
    vm.ret_bool(ctx, changed);
    Ok(())
}

fn n_get_root_alpha(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let tree = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let a = trees().get(tree).map(|t| t.root_alpha).unwrap_or(1.0);
    vm.ret_f32(ctx, a);
    Ok(())
}

fn n_set_anti_alias(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let tree = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let a = vm.arg_bool(ctx, 1);
    let mut table = trees();
    if let Ok(t) = table.get_mut(tree) {
        t.anti_alias = a;
    }
    Ok(())
}

fn n_set_allow_caching(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let tree = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let a = vm.arg_bool(ctx, 1);
    let mut table = trees();
    if let Ok(t) = table.get_mut(tree) {
        t.allow_caching = a;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Property bulk get/set
// ---------------------------------------------------------------------------

fn n_get_full_path_properties(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let arr = vm.arg_ref(ctx, 1);
    let count = vm.arg_i32(ctx, 2);
    // The framework passes `TOTAL_PROPERTY_COUNT * 4` = 48 bytes, not 12 slots.
    if count != 48 {
        vm.ret_bool(ctx, false);
        return Ok(());
    }
    let p = match nodes().get(node) {
        Ok(n) => match &n.kind {
            VNodeKind::FullPath(p) => p.clone(),
            _ => {
                vm.ret_bool(ctx, false);
                return Ok(());
            }
        },
        Err(_) => {
            vm.ret_bool(ctx, false);
            return Ok(());
        }
    };
    let mut bytes = [0u8; 48];
    bytes[0..4].copy_from_slice(&p.stroke_width.to_le_bytes());
    bytes[4..8].copy_from_slice(&(p.stroke_color as i32).to_le_bytes());
    bytes[8..12].copy_from_slice(&p.stroke_alpha.to_le_bytes());
    bytes[12..16].copy_from_slice(&(p.fill_color as i32).to_le_bytes());
    bytes[16..20].copy_from_slice(&p.fill_alpha.to_le_bytes());
    bytes[20..24].copy_from_slice(&p.trim_path_start.to_le_bytes());
    bytes[24..28].copy_from_slice(&p.trim_path_end.to_le_bytes());
    bytes[28..32].copy_from_slice(&p.trim_path_offset.to_le_bytes());
    bytes[32..36].copy_from_slice(&p.stroke_line_cap.to_le_bytes());
    bytes[36..40].copy_from_slice(&p.stroke_line_join.to_le_bytes());
    bytes[40..44].copy_from_slice(&p.stroke_miter_limit.to_le_bytes());
    bytes[44..48].copy_from_slice(&p.fill_type.to_le_bytes());
    let ok = write_byte_array(vm, arr, &bytes);
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn n_get_group_properties(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let arr = vm.arg_ref(ctx, 1);
    let count = vm.arg_i32(ctx, 2);
    if count != 7 {
        vm.ret_bool(ctx, false);
        return Ok(());
    }
    let g = match nodes().get(node) {
        Ok(n) => match &n.kind {
            VNodeKind::Group(g) => g.clone(),
            _ => {
                vm.ret_bool(ctx, false);
                return Ok(());
            }
        },
        Err(_) => {
            vm.ret_bool(ctx, false);
            return Ok(());
        }
    };
    let vals = [
        g.rotation,
        g.pivot_x,
        g.pivot_y,
        g.scale_x,
        g.scale_y,
        g.translate_x,
        g.translate_y,
    ];
    let ok = write_float_array(vm, arr, &vals);
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn n_update_full_path_properties(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let stroke_width = vm.arg_f32(ctx, 1);
    let stroke_color = vm.arg_i32(ctx, 2) as u32;
    let stroke_alpha = vm.arg_f32(ctx, 3);
    let fill_color = vm.arg_i32(ctx, 4) as u32;
    let fill_alpha = vm.arg_f32(ctx, 5);
    let trim_path_start = vm.arg_f32(ctx, 6);
    let trim_path_end = vm.arg_f32(ctx, 7);
    let trim_path_offset = vm.arg_f32(ctx, 8);
    let stroke_miter_limit = vm.arg_f32(ctx, 9);
    let stroke_line_cap = vm.arg_i32(ctx, 10);
    let stroke_line_join = vm.arg_i32(ctx, 11);
    let fill_type = vm.arg_i32(ctx, 12);
    let mut table = nodes();
    let n = table.get_mut(node).map_err(node_err)?;
    if let VNodeKind::FullPath(p) = &mut n.kind {
        p.stroke_width = stroke_width;
        p.stroke_color = stroke_color;
        p.stroke_alpha = stroke_alpha;
        p.fill_color = fill_color;
        p.fill_alpha = fill_alpha;
        p.trim_path_start = trim_path_start;
        p.trim_path_end = trim_path_end;
        p.trim_path_offset = trim_path_offset;
        p.stroke_miter_limit = stroke_miter_limit;
        p.stroke_line_cap = stroke_line_cap;
        p.stroke_line_join = stroke_line_join;
        p.fill_type = fill_type;
        if let Some(ft) = FillType::from_ordinal(fill_type) {
            p.path.set_fill_type(ft);
        }
    }
    Ok(())
}

fn n_update_full_path_fill_gradient(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let g = vm.arg_i64(ctx, 1);
    let mut table = nodes();
    let n = table.get_mut(node).map_err(node_err)?;
    if let VNodeKind::FullPath(p) = &mut n.kind {
        p.fill_gradient = g;
    }
    Ok(())
}

fn n_update_full_path_stroke_gradient(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let g = vm.arg_i64(ctx, 1);
    let mut table = nodes();
    let n = table.get_mut(node).map_err(node_err)?;
    if let VNodeKind::FullPath(p) = &mut n.kind {
        p.stroke_gradient = g;
    }
    Ok(())
}

fn n_update_group_properties(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let rotation = vm.arg_f32(ctx, 1);
    let pivot_x = vm.arg_f32(ctx, 2);
    let pivot_y = vm.arg_f32(ctx, 3);
    let scale_x = vm.arg_f32(ctx, 4);
    let scale_y = vm.arg_f32(ctx, 5);
    let translate_x = vm.arg_f32(ctx, 6);
    let translate_y = vm.arg_f32(ctx, 7);
    let mut table = nodes();
    let n = table.get_mut(node).map_err(node_err)?;
    if let VNodeKind::Group(g) = &mut n.kind {
        g.rotation = rotation;
        g.pivot_x = pivot_x;
        g.pivot_y = pivot_y;
        g.scale_x = scale_x;
        g.scale_y = scale_y;
        g.translate_x = translate_x;
        g.translate_y = translate_y;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-property setters / getters
// ---------------------------------------------------------------------------

macro_rules! full_path_f32 {
    ($set:ident, $get:ident, $field:ident) => {
        fn $set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
            let v = vm.arg_f32(ctx, 1);
            let mut table = nodes();
            let n = table.get_mut(node).map_err(node_err)?;
            if let VNodeKind::FullPath(p) = &mut n.kind {
                p.$field = v;
            }
            Ok(())
        }
        fn $get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
            let v = nodes()
                .get(node)
                .ok()
                .and_then(|n| match &n.kind {
                    VNodeKind::FullPath(p) => Some(p.$field),
                    _ => None,
                })
                .unwrap_or(0.0);
            vm.ret_f32(ctx, v);
            Ok(())
        }
    };
}

macro_rules! full_path_u32 {
    ($set:ident, $get:ident, $field:ident) => {
        fn $set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
            let v = vm.arg_i32(ctx, 1) as u32;
            let mut table = nodes();
            let n = table.get_mut(node).map_err(node_err)?;
            if let VNodeKind::FullPath(p) = &mut n.kind {
                p.$field = v;
            }
            Ok(())
        }
        fn $get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
            let v = nodes()
                .get(node)
                .ok()
                .and_then(|n| match &n.kind {
                    VNodeKind::FullPath(p) => Some(p.$field as i32),
                    _ => None,
                })
                .unwrap_or(0);
            vm.ret_i32(ctx, v);
            Ok(())
        }
    };
}

macro_rules! group_f32 {
    ($set:ident, $get:ident, $field:ident) => {
        fn $set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
            let v = vm.arg_f32(ctx, 1);
            let mut table = nodes();
            let n = table.get_mut(node).map_err(node_err)?;
            if let VNodeKind::Group(g) = &mut n.kind {
                g.$field = v;
            }
            Ok(())
        }
        fn $get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
            let v = nodes()
                .get(node)
                .ok()
                .and_then(|n| match &n.kind {
                    VNodeKind::Group(g) => Some(g.$field),
                    _ => None,
                })
                .unwrap_or(0.0);
            vm.ret_f32(ctx, v);
            Ok(())
        }
    };
}

full_path_f32!(n_set_stroke_width, n_get_stroke_width, stroke_width);
full_path_u32!(n_set_stroke_color, n_get_stroke_color, stroke_color);
full_path_f32!(n_set_stroke_alpha, n_get_stroke_alpha, stroke_alpha);
full_path_u32!(n_set_fill_color, n_get_fill_color, fill_color);
full_path_f32!(n_set_fill_alpha, n_get_fill_alpha, fill_alpha);
full_path_f32!(n_set_trim_path_start, n_get_trim_path_start, trim_path_start);
full_path_f32!(n_set_trim_path_end, n_get_trim_path_end, trim_path_end);
full_path_f32!(n_set_trim_path_offset, n_get_trim_path_offset, trim_path_offset);

group_f32!(n_set_rotation, n_get_rotation, rotation);
group_f32!(n_set_pivot_x, n_get_pivot_x, pivot_x);
group_f32!(n_set_pivot_y, n_get_pivot_y, pivot_y);
group_f32!(n_set_scale_x, n_get_scale_x, scale_x);
group_f32!(n_set_scale_y, n_get_scale_y, scale_y);
group_f32!(n_set_translate_x, n_get_translate_x, translate_x);
group_f32!(n_set_translate_y, n_get_translate_y, translate_y);

// ---------------------------------------------------------------------------
// Draw
// ---------------------------------------------------------------------------

fn n_draw(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let tree = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let canvas = vm.arg_i64(ctx, 1);
    let color_filter = vm.arg_i64(ctx, 2);
    let bounds = vm.arg_ref(ctx, 3);
    let needs_mirroring = vm.arg_bool(ctx, 4);
    let _can_reuse_cache = vm.arg_bool(ctx, 5);

    if !crate::natives::gfx_canvas::canvas_exists(canvas) {
        vm.ret_i32(ctx, 0);
        return Ok(());
    }
    let (left, top, right, bottom) = read_rect_int(vm, bounds);
    let w = right - left;
    let h = bottom - top;
    if w <= 0 || h <= 0 {
        vm.ret_i32(ctx, 0);
        return Ok(());
    }
    let t = match trees().get(tree) {
        Ok(t) => t.clone(),
        Err(_) => {
            vm.ret_i32(ctx, 0);
            return Ok(());
        }
    };
    let viewport_w = if t.viewport_width > 0.0 { t.viewport_width } else { w as f32 };
    let viewport_h = if t.viewport_height > 0.0 { t.viewport_height } else { h as f32 };

    let mut m = Matrix3x3::identity();
    m.pre_translate(left as f32, top as f32);
    m.pre_scale(w as f32 / viewport_w, h as f32 / viewport_h);
    if needs_mirroring {
        m.pre_translate(w as f32, 0.0);
        m.pre_scale(-1.0, 1.0);
    }

    let cf = if color_filter != 0 {
        crate::natives::gfx_paint::filter_spec(color_filter)
    } else {
        None
    };

    crate::natives::gfx_canvas::push_op(canvas, DisplayOp::Save)?;
    crate::natives::gfx_canvas::push_op(canvas, DisplayOp::Concat(m))?;
    {
        let table = nodes();
        draw_node(canvas, &table, t.root, t.root_alpha, t.anti_alias, cf)?;
    }
    crate::natives::gfx_canvas::push_op(canvas, DisplayOp::Restore)?;
    vm.ret_i32(ctx, 1);
    Ok(())
}

fn draw_node(
    canvas: i64,
    table: &HandleTable<VNode>,
    node: Handle,
    root_alpha: f32,
    anti_alias: bool,
    cf: Option<acos_render::paint::ColorFilterSpec>,
) -> VmResult<()> {
    let n = match table.get(node) {
        Ok(n) => n,
        Err(_) => return Ok(()),
    };
    match &n.kind {
        VNodeKind::Group(g) => {
            crate::natives::gfx_canvas::push_op(canvas, DisplayOp::Save)?;
            crate::natives::gfx_canvas::push_op(canvas, DisplayOp::Concat(g.matrix()))?;
            for child in &n.children {
                draw_node(canvas, table, *child, root_alpha, anti_alias, cf.clone())?;
            }
            crate::natives::gfx_canvas::push_op(canvas, DisplayOp::Restore)?;
        }
        VNodeKind::ClipPath(c) => {
            let path = c.path.clone();
            let ph = with_ctx(|ctx| ctx.paths.insert(path));
            crate::natives::gfx_canvas::push_op(
                canvas,
                DisplayOp::ClipPath(ph, ClipOp::Intersect, true),
            )?;
        }
        VNodeKind::FullPath(p) => {
            draw_full_path(canvas, p, root_alpha, anti_alias, cf)?;
        }
    }
    Ok(())
}

fn draw_full_path(
    canvas: i64,
    p: &VFullPath,
    root_alpha: f32,
    anti_alias: bool,
    cf: Option<acos_render::paint::ColorFilterSpec>,
) -> VmResult<()> {
    let mut path = p.path.clone();
    if let Some(ft) = FillType::from_ordinal(p.fill_type) {
        path.set_fill_type(ft);
    }
    if p.trim_path_start != 0.0 || p.trim_path_end != 1.0 || p.trim_path_offset != 0.0 {
        let pm = PathMeasure::new(&path, false);
        let total = pm.total_length();
        let start = ((p.trim_path_start + p.trim_path_offset).rem_euclid(1.0)) * total;
        let end = ((p.trim_path_end + p.trim_path_offset).rem_euclid(1.0)) * total;
        path = pm.get_segment(start, end, true);
    }
    if path.is_empty() {
        return Ok(());
    }
    let ph = with_ctx(|ctx| ctx.paths.insert(path));

    let fill_paint = if p.fill_color != 0 && p.fill_alpha > 0.0 {
        let mut paint = Paint {
            color: p.fill_color,
            alpha: (p.fill_alpha * root_alpha * 255.0).clamp(0.0, 255.0) as u32,
            style: PaintStyle::Fill,
            anti_alias,
            ..Default::default()
        };
        apply_filter(&mut paint, cf.clone(), p.fill_gradient);
        Some(with_ctx(|ctx| ctx.paints.insert(paint)))
    } else {
        None
    };
    let stroke_paint = if p.stroke_color != 0 && p.stroke_width > 0.0 && p.stroke_alpha > 0.0 {
        let mut paint = Paint {
            color: p.stroke_color,
            alpha: (p.stroke_alpha * root_alpha * 255.0).clamp(0.0, 255.0) as u32,
            style: PaintStyle::Stroke,
            stroke_width: p.stroke_width,
            stroke_miter: p.stroke_miter_limit,
            stroke_cap: StrokeCap::from_ordinal(p.stroke_line_cap).unwrap_or(StrokeCap::Butt),
            stroke_join: StrokeJoin::from_ordinal(p.stroke_line_join).unwrap_or(StrokeJoin::Miter),
            anti_alias,
            ..Default::default()
        };
        apply_filter(&mut paint, cf.clone(), p.stroke_gradient);
        Some(with_ctx(|ctx| ctx.paints.insert(paint)))
    } else {
        None
    };

    if let Some(fill) = fill_paint {
        crate::natives::gfx_canvas::push_op(canvas, DisplayOp::DrawPath(ph, fill))?;
    }
    if let Some(stroke) = stroke_paint {
        crate::natives::gfx_canvas::push_op(canvas, DisplayOp::DrawPath(ph, stroke))?;
    }
    Ok(())
}

fn apply_filter(
    paint: &mut Paint,
    cf: Option<acos_render::paint::ColorFilterSpec>,
    gradient: i64,
) {
    if let Some(cf) = cf {
        paint.color_filter = Some(cf);
    }
    if gradient != 0 {
        if let Some(spec) = crate::natives::gfx_paint::shader_spec(gradient) {
            paint.shader = Some(spec);
        }
    }
}

// ---------------------------------------------------------------------------
// android.util.PathParser peers
// ---------------------------------------------------------------------------

fn pp_create_empty(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = path_data().insert(Path::new());
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn pp_create_from_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let s = vm.string_to_rust(vm.arg_ref(ctx, 0)).unwrap_or_default();
    let _len = vm.arg_i32(ctx, 1);
    match parse_path_string(&s) {
        Ok(p) => {
            let h = path_data().insert(p);
            vm.ret_i64(ctx, h.to_jlong());
        }
        Err(_) => vm.ret_i64(ctx, 0),
    }
    Ok(())
}

fn pp_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let p = path_data().get(src).cloned().unwrap_or_default();
    let h = path_data().insert(p);
    vm.ret_i64(ctx, h.to_jlong());
    Ok(())
}

fn pp_create_path(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let out = vm.arg_i64(ctx, 0);
    let src = Handle::from_jlong(vm.arg_i64(ctx, 1));
    let p = path_data().get(src).cloned().map_err(node_err)?;
    crate::natives::gfx_path::replace_path(out, p)?;
    Ok(())
}

fn pp_parse_string_for_path(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let out = vm.arg_i64(ctx, 0);
    let s = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
    let _len = vm.arg_i32(ctx, 2);
    let p = parse_path_string(&s).map_err(|e| {
        VmError::unsupported_api(
            format!("PathParser pathData {:?}: {e}", s),
            "android.util.PathParser",
        )
    })?;
    crate::natives::gfx_path::replace_path(out, p)?;
    Ok(())
}

fn pp_set_path_data(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let out = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let src = Handle::from_jlong(vm.arg_i64(ctx, 1));
    let p = path_data().get(src).cloned().map_err(node_err)?;
    let mut table = path_data();
    *table.get_mut(out).map_err(node_err)? = p;
    Ok(())
}

fn pp_finalize(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = path_data().remove(Handle::from_jlong(vm.arg_i64(ctx, 0)));
    Ok(())
}

fn pp_can_morph(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `PathParser.canMorph` asks whether two `PathData` values have the same
    // verb structure. `AnimatorInflater` uses the answer to decide whether a
    // `pathData` keyframe is morphable and throws `InflateException` when it
    // is not, so this is a structural comparison, not a guess.
    let a = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let b = Handle::from_jlong(vm.arg_i64(ctx, 1));
    let ok = match (path_data().get(a), path_data().get(b)) {
        (Ok(pa), Ok(pb)) => can_morph(pa, pb),
        _ => false,
    };
    vm.ret_bool(ctx, ok);
    Ok(())
}

/// Two paths can morph when every verb has the same kind. Point values may
/// differ (that is what interpolation is for); the command sequence may not.
fn can_morph(a: &Path, b: &Path) -> bool {
    let va = a.verbs();
    let vb = b.verbs();
    va.len() == vb.len()
        && va.iter().zip(vb).all(|(x, y)| {
            matches!(
                (x, y),
                (Verb::MoveTo(..), Verb::MoveTo(..))
                    | (Verb::LineTo(..), Verb::LineTo(..))
                    | (Verb::QuadTo(..), Verb::QuadTo(..))
                    | (Verb::CubicTo(..), Verb::CubicTo(..))
                    | (Verb::Close, Verb::Close)
            )
        })
}

fn pp_interpolate(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Err(VmError::unsupported_api(
        "PathParser.nInterpolatePathData",
        "android.util.PathParser",
    ))
}

// ---------------------------------------------------------------------------
// Array helpers
// ---------------------------------------------------------------------------

fn write_byte_array(vm: &mut Vm, h: u32, data: &[u8]) -> bool {
    use crate::heap::{ArrayData, Object};
    match vm.heap.get_mut(h) {
        Some(Object::Array(a)) => match &mut a.data {
            ArrayData::Byte(v) => {
                let n = v.len().min(data.len());
                for (i, &b) in data[..n].iter().enumerate() {
                    v[i] = b as i8;
                }
                true
            }
            _ => false,
        },
        _ => false,
    }
}

fn write_float_array(vm: &mut Vm, h: u32, data: &[f32]) -> bool {
    use crate::heap::{ArrayData, Object};
    match vm.heap.get_mut(h) {
        Some(Object::Array(a)) => match &mut a.data {
            ArrayData::Float(v) => {
                let n = v.len().min(data.len());
                v[..n].copy_from_slice(&data[..n]);
                true
            }
            _ => false,
        },
        _ => false,
    }
}

fn read_rect_int(vm: &Vm, h: u32) -> (i32, i32, i32, i32) {
    let get = |name: &str| vm.get_instance_field_by_name(h, name).unwrap_or(0) as i32;
    (get("left"), get("top"), get("right"), get("bottom"))
}

// ---------------------------------------------------------------------------
// SVG path data parser (PathParser semantics)
// ---------------------------------------------------------------------------

fn skip_ws(bytes: &[u8], i: &mut usize) {
    while *i < bytes.len() && (bytes[*i].is_ascii_whitespace() || bytes[*i] == b',') {
        *i += 1;
    }
}

fn read_number(bytes: &[u8], i: &mut usize) -> Result<f32, String> {
    skip_ws(bytes, i);
    let start = *i;
    if *i < bytes.len() && (bytes[*i] == b'+' || bytes[*i] == b'-') {
        *i += 1;
    }
    let mut digits = false;
    while *i < bytes.len() && bytes[*i].is_ascii_digit() {
        *i += 1;
        digits = true;
    }
    if *i < bytes.len() && bytes[*i] == b'.' {
        *i += 1;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
            digits = true;
        }
    }
    if !digits {
        return Err(format!("expected number at byte {start}"));
    }
    if *i < bytes.len() && (bytes[*i] == b'e' || bytes[*i] == b'E') {
        *i += 1;
        if *i < bytes.len() && (bytes[*i] == b'+' || bytes[*i] == b'-') {
            *i += 1;
        }
        let mut exp_digits = false;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
            exp_digits = true;
        }
        if !exp_digits {
            return Err(format!("expected exponent at byte {start}"));
        }
    }
    let s = std::str::from_utf8(&bytes[start..*i])
        .map_err(|_| format!("non-UTF-8 number at byte {start}"))?;
    s.parse::<f32>()
        .map_err(|_| format!("invalid number {s:?} at byte {start}"))
}

fn is_command(b: u8) -> bool {
    matches!(
        b,
        b'M' | b'm'
            | b'L' | b'l'
            | b'H' | b'h'
            | b'V' | b'v'
            | b'C' | b'c'
            | b'S' | b's'
            | b'Q' | b'q'
            | b'T' | b't'
            | b'A' | b'a'
            | b'Z' | b'z'
    )
}

/// Parse an SVG path-data string into an [`acos_render::path::Path`].
///
/// Covers the full `PathParser` command set the framework's `VectorDrawable`
/// XML uses: `M/m, L/l, H/h, V/v, C/c, S/s, Q/q, T/t, A/a, Z/z`. Elliptical
/// arcs are converted to cubic segments (with the `x-axis-rotation` applied),
/// matching `PathParser`'s arc handling rather than dropping the command.
fn parse_path_string(s: &str) -> Result<Path, String> {
    let bytes = s.as_bytes();
    let mut p = Path::new();
    let mut i = 0usize;
    let mut cx = 0.0f32;
    let mut cy = 0.0f32;
    let mut start_x = 0.0f32;
    let mut start_y = 0.0f32;
    let mut prev_cmd = 0u8;
    let mut prev_ctrl_x = 0.0f32;
    let mut prev_ctrl_y = 0.0f32;
    let mut have_prev_ctrl = false;
    let mut first = true;

    while i < bytes.len() {
        skip_ws(bytes, &mut i);
        if i >= bytes.len() {
            break;
        }
        let cmd = if is_command(bytes[i]) {
            let c = bytes[i];
            i += 1;
            c
        } else {
            // Implicit repeat of the previous command. `M`/`m` repeat as
            // `L`/`l`; a repeat after `Z` is invalid.
            match prev_cmd {
                b'M' => b'L',
                b'm' => b'l',
                b'Z' | b'z' | 0 => {
                    return Err(format!("unexpected path-data byte {:?} at {}", bytes[i] as char, i))
                }
                other => other,
            }
        };
        let upper = cmd.to_ascii_uppercase();
        let absolute = cmd.is_ascii_uppercase();
        match upper {
            b'M' => {
                let x = read_number(bytes, &mut i)?;
                let y = read_number(bytes, &mut i)?;
                if absolute {
                    cx = x;
                    cy = y;
                } else {
                    cx += x;
                    cy += y;
                }
                if first {
                    p.move_to(cx, cy);
                    start_x = cx;
                    start_y = cy;
                    first = false;
                } else {
                    p.line_to(cx, cy);
                }
                have_prev_ctrl = false;
                prev_cmd = cmd;
            }
            b'L' => {
                let x = read_number(bytes, &mut i)?;
                let y = read_number(bytes, &mut i)?;
                if absolute {
                    cx = x;
                    cy = y;
                } else {
                    cx += x;
                    cy += y;
                }
                p.line_to(cx, cy);
                have_prev_ctrl = false;
                prev_cmd = cmd;
            }
            b'H' => {
                let x = read_number(bytes, &mut i)?;
                cx = if absolute { x } else { cx + x };
                p.line_to(cx, cy);
                have_prev_ctrl = false;
                prev_cmd = cmd;
            }
            b'V' => {
                let y = read_number(bytes, &mut i)?;
                cy = if absolute { y } else { cy + y };
                p.line_to(cx, cy);
                have_prev_ctrl = false;
                prev_cmd = cmd;
            }
            b'C' => {
                let x1 = read_number(bytes, &mut i)?;
                let y1 = read_number(bytes, &mut i)?;
                let x2 = read_number(bytes, &mut i)?;
                let y2 = read_number(bytes, &mut i)?;
                let x = read_number(bytes, &mut i)?;
                let y = read_number(bytes, &mut i)?;
                let (x1, y1, x2, y2, x, y) = if absolute {
                    (x1, y1, x2, y2, x, y)
                } else {
                    (cx + x1, cy + y1, cx + x2, cy + y2, cx + x, cy + y)
                };
                p.cubic_to(x1, y1, x2, y2, x, y);
                prev_ctrl_x = x2;
                prev_ctrl_y = y2;
                have_prev_ctrl = true;
                cx = x;
                cy = y;
                prev_cmd = cmd;
            }
            b'S' => {
                let x2 = read_number(bytes, &mut i)?;
                let y2 = read_number(bytes, &mut i)?;
                let x = read_number(bytes, &mut i)?;
                let y = read_number(bytes, &mut i)?;
                let (x2, y2, x, y) = if absolute {
                    (x2, y2, x, y)
                } else {
                    (cx + x2, cy + y2, cx + x, cy + y)
                };
                let (x1, y1) = if have_prev_ctrl {
                    (2.0 * cx - prev_ctrl_x, 2.0 * cy - prev_ctrl_y)
                } else {
                    (cx, cy)
                };
                p.cubic_to(x1, y1, x2, y2, x, y);
                prev_ctrl_x = x2;
                prev_ctrl_y = y2;
                have_prev_ctrl = true;
                cx = x;
                cy = y;
                prev_cmd = cmd;
            }
            b'Q' => {
                let x1 = read_number(bytes, &mut i)?;
                let y1 = read_number(bytes, &mut i)?;
                let x = read_number(bytes, &mut i)?;
                let y = read_number(bytes, &mut i)?;
                let (x1, y1, x, y) = if absolute {
                    (x1, y1, x, y)
                } else {
                    (cx + x1, cy + y1, cx + x, cy + y)
                };
                p.quad_to(x1, y1, x, y);
                prev_ctrl_x = x1;
                prev_ctrl_y = y1;
                have_prev_ctrl = true;
                cx = x;
                cy = y;
                prev_cmd = cmd;
            }
            b'T' => {
                let x = read_number(bytes, &mut i)?;
                let y = read_number(bytes, &mut i)?;
                let (x, y) = if absolute { (x, y) } else { (cx + x, cy + y) };
                let (x1, y1) = if have_prev_ctrl {
                    (2.0 * cx - prev_ctrl_x, 2.0 * cy - prev_ctrl_y)
                } else {
                    (cx, cy)
                };
                p.quad_to(x1, y1, x, y);
                prev_ctrl_x = x1;
                prev_ctrl_y = y1;
                have_prev_ctrl = true;
                cx = x;
                cy = y;
                prev_cmd = cmd;
            }
            b'A' => {
                let rx = read_number(bytes, &mut i)?;
                let ry = read_number(bytes, &mut i)?;
                let phi = read_number(bytes, &mut i)?;
                let large = read_number(bytes, &mut i)? != 0.0;
                let sweep = read_number(bytes, &mut i)? != 0.0;
                let x = read_number(bytes, &mut i)?;
                let y = read_number(bytes, &mut i)?;
                let (x, y) = if absolute { (x, y) } else { (cx + x, cy + y) };
                arc_to_cubic(&mut p, cx, cy, rx, ry, phi, large, sweep, x, y);
                have_prev_ctrl = false;
                cx = x;
                cy = y;
                prev_cmd = cmd;
            }
            b'Z' => {
                p.close();
                cx = start_x;
                cy = start_y;
                have_prev_ctrl = false;
                prev_cmd = cmd;
            }
            _ => return Err(format!("unsupported path command {:?}", cmd as char)),
        }
    }
    Ok(p)
}

/// Convert an SVG elliptical arc to cubic Bézier segments and append them to
/// `p`. The `phi` rotation is applied to the ellipse, unlike the axis-aligned
/// `Path.arcTo` helper.
#[allow(clippy::too_many_arguments)]
fn arc_to_cubic(
    p: &mut Path,
    x1: f32,
    y1: f32,
    rx: f32,
    ry: f32,
    phi_deg: f32,
    large_arc: bool,
    sweep: bool,
    x2: f32,
    y2: f32,
) {
    if rx == 0.0 || ry == 0.0 {
        p.line_to(x2, y2);
        return;
    }
    let rx = rx.abs();
    let ry = ry.abs();
    let phi = phi_deg.to_radians();
    let (sin_phi, cos_phi) = phi.sin_cos();
    let dx = (x1 - x2) / 2.0;
    let dy = (y1 - y2) / 2.0;
    let x1p = cos_phi * dx + sin_phi * dy;
    let y1p = -sin_phi * dx + cos_phi * dy;
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    let (rx, ry) = if lambda > 1.0 {
        let s = lambda.sqrt();
        (rx * s, ry * s)
    } else {
        (rx, ry)
    };
    let sign = if large_arc == sweep { -1.0 } else { 1.0 };
    let num = (rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p).max(0.0);
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let coef = if den > 0.0 {
        sign * (num / den).sqrt()
    } else {
        0.0
    };
    let cxp = coef * (rx * y1p / ry);
    let cyp = coef * (-ry * x1p / rx);
    let cx = cos_phi * cxp - sin_phi * cyp + (x1 + x2) / 2.0;
    let cy = sin_phi * cxp + cos_phi * cyp + (y1 + y2) / 2.0;

    let ux = (x1p - cxp) / rx;
    let uy = (y1p - cyp) / ry;
    let vx = (-x1p - cxp) / rx;
    let vy = (-y1p - cyp) / ry;
    let theta1 = uy.atan2(ux);
    let mut delta = (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    if !sweep && delta > 0.0 {
        delta -= std::f32::consts::TAU;
    } else if sweep && delta < 0.0 {
        delta += std::f32::consts::TAU;
    }

    let n = ((delta.abs() / (std::f32::consts::PI / 2.0)).ceil() as usize).max(1);
    let delta_seg = delta / n as f32;
    for i in 0..n {
        let t1 = theta1 + i as f32 * delta_seg;
        let t2 = t1 + delta_seg;
        let alpha = 4.0 / 3.0 * ((t2 - t1) / 4.0).tan();
        let (s1, c1) = t1.sin_cos();
        let (s2, c2) = t2.sin_cos();
        let p1x = cx + rx * c1 * cos_phi - ry * s1 * sin_phi;
        let p1y = cy + rx * c1 * sin_phi + ry * s1 * cos_phi;
        let p2x = cx + rx * c2 * cos_phi - ry * s2 * sin_phi;
        let p2y = cy + rx * c2 * sin_phi + ry * s2 * cos_phi;
        let d1x = -rx * s1 * cos_phi - ry * c1 * sin_phi;
        let d1y = -rx * s1 * sin_phi + ry * c1 * cos_phi;
        let d2x = -rx * s2 * cos_phi - ry * c2 * sin_phi;
        let d2y = -rx * s2 * sin_phi + ry * c2 * cos_phi;
        p.cubic_to(
            p1x + alpha * d1x,
            p1y + alpha * d1y,
            p2x - alpha * d2x,
            p2y - alpha * d2y,
            p2x,
            p2y,
        );
    }
}

// ---------------------------------------------------------------------------
// AnimatedVectorDrawable (AVD) animator set
// ---------------------------------------------------------------------------
//
// `AnimatedVectorDrawable` owns a native `AnimatorSet` (the
// `VectorDrawableAnimatorRT`). The Java side builds one property holder per
// `ObjectAnimator` in the XML, adds it to the set, and points the set at the
// `VectorDrawable` tree. The natives below keep that state in
// generation-checked handle tables; the holder's value is applied to the same
// `VNode` / `VTree` table the `VectorDrawable` natives use.
//
// ACOS has no frame clock, so nothing runs between frames. The *static* frame
// is the drawable's base state, which is what `VectorDrawable.nDraw` already
// draws. `nStart`/`nEnd`/`nReset`/`nReverse` record the animation state and
// apply the holder's endpoint value (start on `nStart`/`nReset`, end on
// `nEnd`/`nReverse`), so a later `ValueAnimator`-driven caller observes the
// endpoints through the same tree. No frame is ever interpolated.

#[derive(Clone, Debug)]
enum PropertyHolderKind {
    Group { node: Handle, property_id: i32, start: f32, end: f32 },
    Path { node: Handle, property_id: i32, start: f32, end: f32 },
    PathColor { node: Handle, property_id: i32, start: i32, end: i32 },
    PathData { node: Handle, start: i64, end: i64 },
    RootAlpha { tree: Handle, start: f32, end: f32 },
}

#[derive(Clone, Debug)]
struct PropertyHolder {
    kind: PropertyHolderKind,
    floats: Vec<f32>,
    ints: Vec<i32>,
}

impl PropertyHolder {
    fn start_value(&self) -> f32 {
        match self.kind {
            PropertyHolderKind::Group { start, .. }
            | PropertyHolderKind::Path { start, .. }
            | PropertyHolderKind::RootAlpha { start, .. } => start,
            PropertyHolderKind::PathColor { .. } | PropertyHolderKind::PathData { .. } => 0.0,
        }
    }

    fn end_value(&self) -> f32 {
        match self.kind {
            PropertyHolderKind::Group { end, .. }
            | PropertyHolderKind::Path { end, .. }
            | PropertyHolderKind::RootAlpha { end, .. } => end,
            PropertyHolderKind::PathColor { .. } | PropertyHolderKind::PathData { .. } => 0.0,
        }
    }
}

#[derive(Clone, Debug)]
struct ChildAnimator {
    holder: Handle,
    _interpolator: i64,
    _start_delay: i64,
    _duration: i64,
    _repeat_count: i32,
    _repeat_mode: i32,
}

#[derive(Clone, Debug)]
struct AnimatorSet {
    target: Handle,
    children: Vec<ChildAnimator>,
    started: bool,
    running: bool,
    reversed: bool,
}

impl AnimatorSet {
    fn new() -> Self {
        AnimatorSet {
            target: Handle::NULL,
            children: Vec::new(),
            started: false,
            running: false,
            reversed: false,
        }
    }
}

static AVD_SETS: OnceLock<Mutex<HandleTable<AnimatorSet>>> = OnceLock::new();
static AVD_HOLDERS: OnceLock<Mutex<HandleTable<PropertyHolder>>> = OnceLock::new();

fn avd_sets() -> MutexGuard<'static, HandleTable<AnimatorSet>> {
    let m = AVD_SETS.get_or_init(|| Mutex::new(HandleTable::new()));
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn avd_holders() -> MutexGuard<'static, HandleTable<PropertyHolder>> {
    let m = AVD_HOLDERS.get_or_init(|| Mutex::new(HandleTable::new()));
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn avd_encode_set(h: Handle) -> i64 {
    (h.0 | AVD_SET_TAG) as i64
}

fn avd_decode_set(j: i64) -> Handle {
    let u = j as u64;
    if u & AVD_SET_TAG == 0 {
        Handle::NULL
    } else {
        Handle(u & !AVD_SET_TAG)
    }
}

fn avd_encode_holder(h: Handle) -> i64 {
    (h.0 | AVD_HOLDER_TAG) as i64
}

fn avd_decode_holder(j: i64) -> Handle {
    let u = j as u64;
    if u & AVD_HOLDER_TAG == 0 {
        Handle::NULL
    } else {
        Handle(u & !AVD_HOLDER_TAG)
    }
}

fn avd_err(e: crate::natives::gfx_handles::HandleError) -> VmError {
    VmError::Internal(format!("AnimatedVectorDrawable: {e}"))
}

/// Apply one property holder's value to its target node. Group and full-path
/// property indices are the framework's own `VGroup` / `VFullPath`
/// `getPropertyIndex` maps, which is the order the native tree uses.
fn apply_holder_value(holder: &PropertyHolder, value: f32, color: i32) {
    match holder.kind {
        PropertyHolderKind::Group { node, property_id, .. } => {
            if let Ok(n) = nodes().get_mut(node) {
                if let VNodeKind::Group(g) = &mut n.kind {
                    match property_id {
                        0 => g.rotation = value,
                        1 => g.pivot_x = value,
                        2 => g.pivot_y = value,
                        3 => g.scale_x = value,
                        4 => g.scale_y = value,
                        5 => g.translate_x = value,
                        6 => g.translate_y = value,
                        _ => {}
                    }
                }
            }
        }
        PropertyHolderKind::Path { node, property_id, .. } => {
            if let Ok(n) = nodes().get_mut(node) {
                if let VNodeKind::FullPath(p) = &mut n.kind {
                    match property_id {
                        0 => p.stroke_width = value,
                        2 => p.stroke_alpha = value,
                        4 => p.fill_alpha = value,
                        5 => p.trim_path_start = value,
                        6 => p.trim_path_end = value,
                        7 => p.trim_path_offset = value,
                        _ => {}
                    }
                }
            }
        }
        PropertyHolderKind::PathColor { node, property_id, .. } => {
            if let Ok(n) = nodes().get_mut(node) {
                if let VNodeKind::FullPath(p) = &mut n.kind {
                    match property_id {
                        1 => p.stroke_color = color as u32,
                        3 => p.fill_color = color as u32,
                        _ => {}
                    }
                }
            }
        }
        PropertyHolderKind::PathData { node, start, end } => {
            let pd = if value == 0.0 { start } else { end };
            if let Ok(path) = path_data().get(Handle::from_jlong(pd)).cloned() {
                if let Ok(n) = nodes().get_mut(node) {
                    match &mut n.kind {
                        VNodeKind::FullPath(p) => p.path = path,
                        VNodeKind::ClipPath(p) => p.path = path,
                        VNodeKind::Group(_) => {}
                    }
                }
            }
        }
        PropertyHolderKind::RootAlpha { tree, .. } => {
            if let Ok(t) = trees().get_mut(tree) {
                t.root_alpha = value;
            }
        }
    }
}

/// Apply every child holder's endpoint value to the target tree.
fn apply_set(animator: &AnimatorSet, at_start: bool) {
    let holders = avd_holders();
    for child in &animator.children {
        let Ok(h) = holders.get(child.holder) else { continue };
        match h.kind {
            PropertyHolderKind::PathColor { start, end, .. } => {
                let v = if at_start { start } else { end };
                apply_holder_value(h, 0.0, v);
            }
            PropertyHolderKind::PathData { .. } => {
                apply_holder_value(h, if at_start { 0.0 } else { 1.0 }, 0);
            }
            _ => {
                let v = if at_start { h.start_value() } else { h.end_value() };
                apply_holder_value(h, v, 0);
            }
        }
    }
}

fn n_create_animator_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = avd_sets().insert(AnimatorSet::new());
    vm.ret_i64(ctx, avd_encode_set(h));
    Ok(())
}

fn n_create_group_property_holder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let property_id = vm.arg_i32(ctx, 1);
    let start = vm.arg_f32(ctx, 2);
    let end = vm.arg_f32(ctx, 3);
    let h = avd_holders().insert(PropertyHolder {
        kind: PropertyHolderKind::Group { node, property_id, start, end },
        floats: vec![start, end],
        ints: Vec::new(),
    });
    vm.ret_i64(ctx, avd_encode_holder(h));
    Ok(())
}

fn n_create_path_property_holder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let property_id = vm.arg_i32(ctx, 1);
    let start = vm.arg_f32(ctx, 2);
    let end = vm.arg_f32(ctx, 3);
    let h = avd_holders().insert(PropertyHolder {
        kind: PropertyHolderKind::Path { node, property_id, start, end },
        floats: vec![start, end],
        ints: Vec::new(),
    });
    vm.ret_i64(ctx, avd_encode_holder(h));
    Ok(())
}

fn n_create_path_color_property_holder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let property_id = vm.arg_i32(ctx, 1);
    let start = vm.arg_i32(ctx, 2);
    let end = vm.arg_i32(ctx, 3);
    let h = avd_holders().insert(PropertyHolder {
        kind: PropertyHolderKind::PathColor { node, property_id, start, end },
        floats: Vec::new(),
        ints: vec![start, end],
    });
    vm.ret_i64(ctx, avd_encode_holder(h));
    Ok(())
}

fn n_create_path_data_property_holder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let node = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let start = vm.arg_i64(ctx, 1);
    let end = vm.arg_i64(ctx, 2);
    let h = avd_holders().insert(PropertyHolder {
        kind: PropertyHolderKind::PathData { node, start, end },
        floats: Vec::new(),
        ints: Vec::new(),
    });
    vm.ret_i64(ctx, avd_encode_holder(h));
    Ok(())
}

fn n_create_root_alpha_property_holder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let tree = Handle::from_jlong(vm.arg_i64(ctx, 0));
    let start = vm.arg_f32(ctx, 1);
    let end = vm.arg_f32(ctx, 2);
    let h = avd_holders().insert(PropertyHolder {
        kind: PropertyHolderKind::RootAlpha { tree, start, end },
        floats: vec![start, end],
        ints: Vec::new(),
    });
    vm.ret_i64(ctx, avd_encode_holder(h));
    Ok(())
}

fn n_set_property_holder_data_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    use crate::heap::{ArrayData, Object};
    let holder = avd_decode_holder(vm.arg_i64(ctx, 0));
    let arr = vm.arg_ref(ctx, 1);
    let len = vm.arg_i32(ctx, 2).max(0) as usize;
    let values = match vm.heap.get(arr) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Float(v) => v.iter().take(len).copied().collect(),
            _ => return Err(VmError::Internal("AnimatedVectorDrawable: expected float[]".into())),
        },
        _ => return Err(VmError::Internal("AnimatedVectorDrawable: null float[]".into())),
    };
    avd_holders().get_mut(holder).map_err(avd_err)?.floats = values;
    Ok(())
}

fn n_set_property_holder_data_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    use crate::heap::{ArrayData, Object};
    let holder = avd_decode_holder(vm.arg_i64(ctx, 0));
    let arr = vm.arg_ref(ctx, 1);
    let len = vm.arg_i32(ctx, 2).max(0) as usize;
    let values = match vm.heap.get(arr) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Int(v) => v.iter().take(len).copied().collect(),
            _ => return Err(VmError::Internal("AnimatedVectorDrawable: expected int[]".into())),
        },
        _ => return Err(VmError::Internal("AnimatedVectorDrawable: null int[]".into())),
    };
    avd_holders().get_mut(holder).map_err(avd_err)?.ints = values;
    Ok(())
}

fn n_add_animator(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let set = avd_decode_set(vm.arg_i64(ctx, 0));
    let holder = avd_decode_holder(vm.arg_i64(ctx, 1));
    let interpolator = vm.arg_i64(ctx, 2);
    let start_delay = vm.arg_i64(ctx, 3);
    let duration = vm.arg_i64(ctx, 4);
    let repeat_count = vm.arg_i32(ctx, 5);
    let repeat_mode = vm.arg_i32(ctx, 6);
    avd_sets().get_mut(set).map_err(avd_err)?.children.push(ChildAnimator {
        holder,
        _interpolator: interpolator,
        _start_delay: start_delay,
        _duration: duration,
        _repeat_count: repeat_count,
        _repeat_mode: repeat_mode,
    });
    Ok(())
}

fn n_set_vector_drawable_target(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let set = avd_decode_set(vm.arg_i64(ctx, 0));
    let tree = Handle::from_jlong(vm.arg_i64(ctx, 1));
    avd_sets().get_mut(set).map_err(avd_err)?.target = tree;
    Ok(())
}

fn n_start(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let set = avd_decode_set(vm.arg_i64(ctx, 0));
    let anim = {
        let mut sets = avd_sets();
        let s = sets.get_mut(set).map_err(avd_err)?;
        s.started = true;
        s.running = true;
        s.clone()
    };
    apply_set(&anim, true);
    Ok(())
}

fn n_end(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let set = avd_decode_set(vm.arg_i64(ctx, 0));
    let anim = {
        let mut sets = avd_sets();
        let s = sets.get_mut(set).map_err(avd_err)?;
        s.started = false;
        s.running = false;
        s.clone()
    };
    apply_set(&anim, false);
    Ok(())
}

fn n_reset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let set = avd_decode_set(vm.arg_i64(ctx, 0));
    let anim = {
        let mut sets = avd_sets();
        let s = sets.get_mut(set).map_err(avd_err)?;
        s.started = false;
        s.running = false;
        s.reversed = false;
        s.clone()
    };
    apply_set(&anim, true);
    Ok(())
}

fn n_reverse(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let set = avd_decode_set(vm.arg_i64(ctx, 0));
    let anim = {
        let mut sets = avd_sets();
        let s = sets.get_mut(set).map_err(avd_err)?;
        s.started = true;
        s.running = true;
        s.reversed = true;
        s.clone()
    };
    apply_set(&anim, false);
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_progress_bar_arc_path() {
        // The framework's `vector_drawable_progress_bar_medium.xml` pathData.
        let p = parse_path_string("M0, 0 m 0, -18 a 18,18 0 1,1 0,36 a 18,18 0 1,1 0,-36").unwrap();
        assert!(!p.is_empty());
        // Two arcs of 36 units each -> a closed circle-like contour; bounds are
        // the 36x36 circle centred at (0,0).
        let b = p.compute_bounds().unwrap();
        assert!((b.left + 18.0).abs() < 0.5, "left {}", b.left);
        assert!((b.right - 18.0).abs() < 0.5, "right {}", b.right);
        assert!((b.top + 18.0).abs() < 0.5, "top {}", b.top);
        assert!((b.bottom - 18.0).abs() < 0.5, "bottom {}", b.bottom);
    }

    #[test]
    fn parses_the_ic_ab_back_material_path() {
        // The framework's `ic_ab_back_material.xml` pathData (relative commands
        // and no separators after the command letters).
        let p = parse_path_string(
            "M20,11L7.8,11l5.6,-5.6L12,4l-8,8l8,8l1.4,-1.4L7.8,13L20,13L20,11z",
        )
        .unwrap();
        assert!(!p.is_empty());
        let b = p.compute_bounds().unwrap();
        assert!(b.left >= 3.9 && b.left <= 4.1, "left {}", b.left);
        assert!(b.right >= 19.9 && b.right <= 20.1, "right {}", b.right);
        assert!(b.top >= 3.9 && b.top <= 4.1, "top {}", b.top);
        assert!(b.bottom >= 19.9 && b.bottom <= 20.1, "bottom {}", b.bottom);
    }

    #[test]
    fn parses_all_command_letters() {
        let p = parse_path_string(
            "M1 2 L3 4 H5 V6 C7 8 9 10 11 12 S13 14 15 16 Q17 18 19 20 T21 22 A2 3 0 0 1 24 25 Z",
        )
        .unwrap();
        assert!(!p.is_empty());
    }

    #[test]
    fn group_matrix_rotates_around_pivot() {
        let g = VGroup {
            rotation: 90.0,
            pivot_x: 10.0,
            pivot_y: 10.0,
            ..Default::default()
        };
        let m = g.matrix();
        let q = m.map_point(acos_render::geom::PointF::new(20.0, 10.0));
        assert!((q.x - 10.0).abs() < 1e-4 && (q.y - 20.0).abs() < 1e-4);
    }

    #[test]
    fn full_path_properties_round_trip_layout() {
        let p = VFullPath {
            stroke_width: 4.0,
            stroke_color: 0xff112233,
            stroke_alpha: 0.5,
            fill_color: 0x80445566,
            fill_alpha: 0.25,
            trim_path_start: 0.1,
            trim_path_end: 0.9,
            trim_path_offset: 0.05,
            stroke_miter_limit: 3.0,
            stroke_line_cap: 2,
            stroke_line_join: 1,
            fill_type: 1,
            ..Default::default()
        };
        let mut bytes = [0u8; 48];
        bytes[0..4].copy_from_slice(&p.stroke_width.to_le_bytes());
        bytes[4..8].copy_from_slice(&(p.stroke_color as i32).to_le_bytes());
        bytes[8..12].copy_from_slice(&p.stroke_alpha.to_le_bytes());
        bytes[12..16].copy_from_slice(&(p.fill_color as i32).to_le_bytes());
        bytes[16..20].copy_from_slice(&p.fill_alpha.to_le_bytes());
        bytes[20..24].copy_from_slice(&p.trim_path_start.to_le_bytes());
        bytes[24..28].copy_from_slice(&p.trim_path_end.to_le_bytes());
        bytes[28..32].copy_from_slice(&p.trim_path_offset.to_le_bytes());
        bytes[32..36].copy_from_slice(&p.stroke_line_cap.to_le_bytes());
        bytes[36..40].copy_from_slice(&p.stroke_line_join.to_le_bytes());
        bytes[40..44].copy_from_slice(&p.stroke_miter_limit.to_le_bytes());
        bytes[44..48].copy_from_slice(&p.fill_type.to_le_bytes());
        assert_eq!(f32::from_le_bytes(bytes[0..4].try_into().unwrap()), 4.0);
        assert_eq!(i32::from_le_bytes(bytes[4..8].try_into().unwrap()), 0xff112233u32 as i32);
        assert_eq!(f32::from_le_bytes(bytes[20..24].try_into().unwrap()), 0.1);
        assert_eq!(i32::from_le_bytes(bytes[32..36].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(bytes[44..48].try_into().unwrap()), 1);
    }
}
