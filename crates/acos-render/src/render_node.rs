//! `android.graphics.RenderNode` model + display lists + damage tracking.
//!
//! Each node carries Android's property set (bounds, translation, scale,
//! rotation, pivot, alpha, clip, outline, elevation, layer type) and a
//! `DisplayOp` display list. A node's `version` bumps on every mutation, so the
//! renderer can detect that an unchanged UI needs no rasterisation at all —
//! the cached frame is simply re-presented.

use crate::canvas::ClipOp;
use crate::geom::{Outline, RectF};
use crate::handle::Handle;
use crate::matrix::Matrix3x3;

/// A recorded canvas operation. This is the ACOS display-list IR; the
/// framework's `RecordingCanvas.nDraw*` natives append to it.
#[derive(Clone, PartialEq, Debug)]
pub enum DisplayOp {
    Save,
    Restore,
    SaveLayerAlpha(Option<RectF>, u8),
    Concat(Matrix3x3),
    SetMatrix(Matrix3x3),
    ClipRect(RectF, ClipOp, bool),
    ClipPath(Handle, ClipOp, bool),
    DrawColor(u32),
    DrawRect(RectF, Handle),
    DrawRoundRect(RectF, f32, f32, Handle),
    DrawOval(RectF, Handle),
    DrawCircle(f32, f32, f32, Handle),
    DrawArc(RectF, f32, f32, bool, Handle),
    DrawLine(f32, f32, f32, f32, Handle),
    DrawPoint(f32, f32, Handle),
    DrawPath(Handle, Handle),
    DrawBitmap(Handle, Option<RectF>, RectF, Option<Handle>, bool),
    DrawNinePatch(
        Handle,
        crate::geom::NinePatchChunk,
        crate::geom::Rect,
        Handle,
        bool,
    ),
    DrawText(Vec<u16>, f32, f32, Handle),
    DrawRenderNode(Handle),
}

/// Android `RenderNode.LayerType`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum LayerType {
    None = 0,
    Software = 1,
    Hardware = 2,
}

impl LayerType {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            0 => LayerType::None,
            1 => LayerType::Software,
            2 => LayerType::Hardware,
            _ => return None,
        })
    }
}

/// A render node.
#[derive(Clone)]
pub struct RenderNode {
    pub name: String,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub translation_x: f32,
    pub translation_y: f32,
    pub translation_z: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotation: f32,
    pub rotation_x: f32,
    pub rotation_y: f32,
    pub pivot_x: f32,
    pub pivot_y: f32,
    pub pivot_explicit: bool,
    pub alpha: f32,
    pub clip_to_bounds: bool,
    pub clip_to_outline: bool,
    pub outline: Outline,
    pub elevation: f32,
    pub ambient_shadow_color: u32,
    pub spot_shadow_color: u32,
    pub has_overlapping_rendering: bool,
    pub layer_type: LayerType,
    pub layer_paint: Option<Handle>,
    /// Node-level `RenderEffect` applied to the node's offscreen layer.
    pub render_effect: Option<crate::paint::RenderEffectSpec>,
    pub display_list: Vec<DisplayOp>,
    pub children: Vec<Handle>,
    /// Mutation counter for damage tracking.
    pub version: u64,
    /// Number of draw ops actually rasterised for this node (diagnostics).
    pub raster_ops: u64,
}

impl RenderNode {
    pub fn new(name: impl Into<String>) -> Self {
        RenderNode {
            name: name.into(),
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
            translation_x: 0.0,
            translation_y: 0.0,
            translation_z: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation: 0.0,
            rotation_x: 0.0,
            rotation_y: 0.0,
            pivot_x: 0.0,
            pivot_y: 0.0,
            pivot_explicit: false,
            alpha: 1.0,
            clip_to_bounds: false,
            clip_to_outline: false,
            outline: Outline::None,
            elevation: 0.0,
            // Android ambient/spot shadows are translucent black; full opacity
            // would paint an opaque rectangle behind every elevated node.
            ambient_shadow_color: 0x4000_0000,
            spot_shadow_color: 0x5900_0000,
            has_overlapping_rendering: true,
            layer_type: LayerType::None,
            layer_paint: None,
            render_effect: None,
            display_list: Vec::new(),
            children: Vec::new(),
            version: 1,
            raster_ops: 0,
        }
    }

    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1).max(1);
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn set_left_top_right_bottom(&mut self, l: i32, t: i32, r: i32, b: i32) {
        self.left = l;
        self.top = t;
        self.right = r;
        self.bottom = b;
        self.bump();
    }

    pub fn set_alpha(&mut self, alpha: f32) {
        self.alpha = alpha.clamp(0.0, 1.0);
        self.bump();
    }

    pub fn set_translation(&mut self, x: f32, y: f32) {
        self.translation_x = x;
        self.translation_y = y;
        self.bump();
    }

    pub fn set_translation_z(&mut self, z: f32) {
        self.translation_z = z;
        self.bump();
    }

    pub fn set_scale(&mut self, x: f32, y: f32) {
        self.scale_x = x;
        self.scale_y = y;
        self.bump();
    }

    pub fn set_rotation(&mut self, r: f32) {
        self.rotation = r;
        self.bump();
    }

    pub fn set_rotation_xyz(&mut self, x: f32, y: f32, z: f32) {
        self.rotation_x = x;
        self.rotation_y = y;
        self.rotation = z;
        self.bump();
    }

    pub fn set_pivot(&mut self, x: f32, y: f32) {
        self.pivot_x = x;
        self.pivot_y = y;
        self.pivot_explicit = true;
        self.bump();
    }

    pub fn set_clip_to_bounds(&mut self, v: bool) {
        self.clip_to_bounds = v;
        self.bump();
    }

    pub fn set_clip_to_outline(&mut self, v: bool) {
        self.clip_to_outline = v;
        self.bump();
    }

    pub fn set_outline_round_rect(&mut self, l: i32, t: i32, r: i32, b: i32, rx: f32, ry: f32) {
        self.outline = Outline::RoundRect {
            rect: RectF::new(l as f32, t as f32, r as f32, b as f32),
            radius_x: rx,
            radius_y: ry,
        };
        self.bump();
    }

    pub fn set_outline_path(&mut self, path: crate::path::Path, alpha: f32) {
        self.outline = Outline::Path { path, alpha };
        self.bump();
    }

    pub fn set_outline_rect(&mut self, r: RectF) {
        self.outline = Outline::Rect(r);
        self.bump();
    }

    pub fn set_outline_none(&mut self) {
        self.outline = Outline::None;
        self.bump();
    }

    pub fn set_elevation(&mut self, e: f32) {
        self.elevation = e.max(0.0);
        self.bump();
    }

    pub fn set_has_overlapping_rendering(&mut self, v: bool) {
        self.has_overlapping_rendering = v;
        self.bump();
    }

    pub fn set_layer_type(&mut self, t: LayerType) {
        self.layer_type = t;
        self.bump();
    }

    pub fn set_layer_paint(&mut self, paint: Option<Handle>) {
        self.layer_paint = paint;
        self.bump();
    }

    pub fn add_child(&mut self, child: Handle) {
        if !self.children.contains(&child) {
            self.children.push(child);
        }
        self.bump();
    }

    pub fn remove_child(&mut self, child: Handle) {
        self.children.retain(|c| *c != child);
        self.bump();
    }

    pub fn set_display_list(&mut self, ops: Vec<DisplayOp>) {
        self.display_list = ops;
        self.bump();
    }

    pub fn push_op(&mut self, op: DisplayOp) {
        self.display_list.push(op);
        self.bump();
    }

    pub fn clear_display_list(&mut self) {
        self.display_list.clear();
        self.bump();
    }

    /// The node's local→parent transform (Android's RenderNode property
    /// composition, evaluated at `top,left`).
    pub fn local_matrix(&self) -> Matrix3x3 {
        let mut m = Matrix3x3::identity();
        m.set_translate(
            self.left as f32 + self.translation_x,
            self.top as f32 + self.translation_y,
        );
        let px = if self.pivot_explicit {
            self.pivot_x
        } else {
            self.width() as f32 * 0.5
        };
        let py = if self.pivot_explicit {
            self.pivot_y
        } else {
            self.height() as f32 * 0.5
        };
        if self.rotation != 0.0 || self.scale_x != 1.0 || self.scale_y != 1.0 {
            let mut local = Matrix3x3::identity();
            local.set_translate(px, py);
            local.pre_concat(&Matrix3x3::rotate_matrix(self.rotation));
            local.pre_concat(&Matrix3x3::scale_matrix(self.scale_x, self.scale_y));
            local.pre_concat(&Matrix3x3::translate_matrix(-px, -py));
            m.pre_concat(&local);
        }
        m
    }

    pub fn has_identity_matrix(&self) -> bool {
        self.local_matrix().is_identity()
    }

    /// Conservative damage/version digest for a subtree.
    pub fn subtree_version(nodes: &crate::handle::HandleTable<RenderNode>, root: Handle) -> u64 {
        let mut acc: u64 = 0;
        let mut stack = vec![root];
        while let Some(h) = stack.pop() {
            if let Some(n) = nodes.get(h) {
                acc = acc
                    .wrapping_mul(1099511628211)
                    .wrapping_add(n.version);
                for c in &n.children {
                    stack.push(*c);
                }
            }
        }
        acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::HandleTable;

    #[test]
    fn version_bumps_on_mutation() {
        let mut n = RenderNode::new("x");
        let v0 = n.version;
        n.set_alpha(0.5);
        assert!(n.version > v0);
    }

    #[test]
    fn subtree_version_changes_when_child_changes() {
        let mut table: HandleTable<RenderNode> = HandleTable::new();
        let child = table.insert(RenderNode::new("child"));
        let mut root = RenderNode::new("root");
        root.add_child(child);
        let root_h = table.insert(root);
        let v1 = RenderNode::subtree_version(&table, root_h);
        table.get_mut(child).unwrap().set_alpha(0.3);
        let v2 = RenderNode::subtree_version(&table, root_h);
        assert_ne!(v1, v2);
    }

    #[test]
    fn local_matrix_translates_by_bounds() {
        let mut n = RenderNode::new("x");
        n.set_left_top_right_bottom(10, 20, 110, 220);
        let p = n.local_matrix().map_point(crate::geom::PointF::new(0.0, 0.0));
        assert_eq!((p.x, p.y), (10.0, 20.0));
    }
}
