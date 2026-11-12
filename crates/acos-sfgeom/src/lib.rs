//! Safe Rust API over the vendored Android 14 SurfaceFlinger geometry kernel.
//!
//! The geometry itself is Google's code (`third_party/aosp/`, provenance in
//! `third_party/aosp/PROVENANCE.md`): `ui::Transform` / `ui::Rect` and the
//! `RequestedLayerState` + `LayerSnapshotBuilder` transform/bounds bodies, copied
//! verbatim and adapted only in their state types. See `cpp/sfgeom_kernel.cpp`.
//!
//! This crate deliberately has no knowledge of buffers, fences, binder or C1: a
//! layer is a POD record and a buffer is a `(width, height)` pair. It is the
//! "which pixels go where" half of the compositor only (SPIKE-B.md §6).

/// Rect with no area, the encoding used for "not set" (`ui::Rect::INVALID_RECT`).
const EMPTY_RECT: [i32; 4] = [0, 0, -1, -1];

/// A display, used only for `getUnrotatedBufferSize` rotation and the synthetic
/// root bounds (`LayerSnapshotBuilder.cpp:44-62`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Display {
    pub width: u32,
    pub height: u32,
    /// `ui::Transform::RotationFlags`: `ROT_0 = 0`, `FLIP_H = 1`, `FLIP_V = 2`,
    /// `ROT_90 = 4`, `ROT_180 = 3`, `ROT_270 = 7`.
    pub rotation_flags: u32,
}

impl Display {
    pub const fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            rotation_flags: 0,
        }
    }

    pub const fn with_rotation(width: u32, height: u32, rotation_flags: u32) -> Self {
        Self {
            width,
            height,
            rotation_flags,
        }
    }
}

/// One layer's geometry input, the subset of `layer_state_t` /
/// `RequestedLayerState` that the stock geometry algebra reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerIn {
    pub id: i32,
    /// Parent layer id, or `None` for a top-level layer (hangs off the synthetic root).
    pub parent: Option<i32>,
    pub z: i32,
    pub x: f32,
    pub y: f32,
    /// `layer_state_t` matrix, in wire order: `dsdx, dtdx, dtdy, dsdy`.
    pub matrix: [f32; 4],
    /// `{left, top, right, bottom}`; `None` selects the position/matrix path
    /// (`RequestedLayerState.cpp:363`).
    pub destination_frame: Option<[i32; 4]>,
    /// Window crop (`layer_state_t::crop`). `None` = unset.
    pub crop: Option<[i32; 4]>,
    /// Buffer crop. Kept distinct from [`Self::crop`], as stock does
    /// (`RequestedLayerState.cpp:481-494`).
    pub buffer_crop: Option<[i32; 4]>,
    /// Buffer size in pixels; `None` means no buffer (a solid-colour/parent-bounded layer).
    pub buffer_size: Option<(u32, u32)>,
    /// `ui::Transform::RotationFlags` applied to the buffer.
    pub buffer_transform: u32,
    pub transform_to_display_inverse: bool,
    pub alpha: f32,
    pub hidden: bool,
}

impl Default for LayerIn {
    fn default() -> Self {
        Self {
            id: 0,
            parent: None,
            z: 0,
            x: 0.0,
            y: 0.0,
            matrix: [1.0, 0.0, 0.0, 1.0],
            destination_frame: None,
            crop: None,
            buffer_crop: None,
            buffer_size: None,
            buffer_transform: 0,
            transform_to_display_inverse: false,
            alpha: 1.0,
            hidden: false,
        }
    }
}

/// One layer's computed geometry, mirroring the `LayerSnapshot` fields stock
/// `CompositionEngine` consumes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snapshot {
    pub id: i32,
    /// Composed `parent * local` transform (`ui::Transform` mat33, column-major:
    /// `geom[col * 3 + row]`).
    pub geom_transform: [f32; 9],
    /// `{left, top, right, bottom}` of the transformed, parent-clipped layer bounds.
    pub transformed_bounds: [f32; 4],
    /// Layer bounds in the layer's own coordinate space after parent clipping.
    pub geom_layer_bounds: [f32; 4],
    /// Buffer size intersected with the window crop (`RequestedLayerState.cpp:471`).
    pub cropped_buffer_size: [i32; 4],
    /// Buffer crop intersected with the buffer bounds; never conflated with `crop`.
    pub buffer_crop: [i32; 4],
    /// `parent.alpha * requested.alpha` down the tree (`LayerSnapshotBuilder.cpp:757`).
    pub alpha: f32,
    /// Hidden by its own flag, an ancestor's flag, or an ancestor's invalid transform
    /// (`LayerSnapshotBuilder.cpp:717-719`).
    pub hidden_by_policy: bool,
    /// Zero/infinite/nan determinant, reset to identity (`LayerSnapshot.cpp:261`).
    pub invalid_transform: bool,
    /// Index of this layer in ascending `(z, id)` draw order
    /// (`LayerHierarchy.cpp:28-38`).
    pub draw_order: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct SfDisplay {
    width: u32,
    height: u32,
    rotation_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct SfLayerIn {
    id: i32,
    parent_id: i32,
    z: i32,
    x: f32,
    y: f32,
    dsdx: f32,
    dtdx: f32,
    dtdy: f32,
    dsdy: f32,
    destination_frame: [i32; 4],
    crop: [i32; 4],
    buffer_crop: [i32; 4],
    buffer_width: u32,
    buffer_height: u32,
    buffer_transform: u32,
    transform_to_display_inverse: u8,
    alpha: f32,
    hidden: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct SfSnapshotOut {
    id: i32,
    geom: [f32; 9],
    transformed_bounds: [f32; 4],
    geom_layer_bounds: [f32; 4],
    cropped_buffer_size: [i32; 4],
    buffer_crop: [i32; 4],
    alpha: f32,
    hidden_by_policy: u8,
    invalid_transform: u8,
    draw_order: i32,
}

impl SfSnapshotOut {
    const ZERO: Self = Self {
        id: 0,
        geom: [0.0; 9],
        transformed_bounds: [0.0; 4],
        geom_layer_bounds: [0.0; 4],
        cropped_buffer_size: [0; 4],
        buffer_crop: [0; 4],
        alpha: 0.0,
        hidden_by_policy: 0,
        invalid_transform: 0,
        draw_order: -1,
    };
}

extern "C" {
    fn sfgeom_compute(
        layers: *const SfLayerIn,
        n: usize,
        display: SfDisplay,
        out: *mut SfSnapshotOut,
    );
}

/// Computes one [`Snapshot`] per input layer, returned in the same order as `layers`.
///
/// `parent` ids must name a layer in the same slice (or be `None`).
pub fn compute(layers: &[LayerIn], display: Display) -> Vec<Snapshot> {
    if layers.is_empty() {
        return Vec::new();
    }

    let inputs: Vec<SfLayerIn> = layers.iter().map(SfLayerIn::from).collect();
    let mut outputs = vec![SfSnapshotOut::ZERO; inputs.len()];

    // SAFETY: `inputs` and `outputs` are both `layers.len()` elements, the structs are
    // #[repr(C)] mirrors of the C ABI in cpp/sfgeom_kernel.h, and the callee only reads
    // the input array and writes exactly that many output elements.
    unsafe {
        sfgeom_compute(
            inputs.as_ptr(),
            inputs.len(),
            SfDisplay {
                width: display.width,
                height: display.height,
                rotation_flags: display.rotation_flags,
            },
            outputs.as_mut_ptr(),
        );
    }

    outputs.iter().map(Snapshot::from).collect()
}

impl From<&LayerIn> for SfLayerIn {
    fn from(layer: &LayerIn) -> Self {
        let [dsdx, dtdx, dtdy, dsdy] = layer.matrix;
        let (buffer_width, buffer_height) = layer.buffer_size.unwrap_or((0, 0));
        Self {
            id: layer.id,
            parent_id: layer.parent.unwrap_or(-1),
            z: layer.z,
            x: layer.x,
            y: layer.y,
            dsdx,
            dtdx,
            dtdy,
            dsdy,
            destination_frame: layer.destination_frame.unwrap_or(EMPTY_RECT),
            crop: layer.crop.unwrap_or(EMPTY_RECT),
            buffer_crop: layer.buffer_crop.unwrap_or(EMPTY_RECT),
            buffer_width,
            buffer_height,
            buffer_transform: layer.buffer_transform,
            transform_to_display_inverse: u8::from(layer.transform_to_display_inverse),
            alpha: layer.alpha,
            hidden: u8::from(layer.hidden),
        }
    }
}

impl From<&SfSnapshotOut> for Snapshot {
    fn from(out: &SfSnapshotOut) -> Self {
        Self {
            id: out.id,
            geom_transform: out.geom,
            transformed_bounds: out.transformed_bounds,
            geom_layer_bounds: out.geom_layer_bounds,
            cropped_buffer_size: out.cropped_buffer_size,
            buffer_crop: out.buffer_crop,
            alpha: out.alpha,
            hidden_by_policy: out.hidden_by_policy != 0,
            invalid_transform: out.invalid_transform != 0,
            draw_order: out.draw_order.max(0) as usize,
        }
    }
}
