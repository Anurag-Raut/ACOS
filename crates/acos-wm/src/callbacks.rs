//! The outgoing `IWindow` callback queue.
//!
//! WMS tells the application about state changes by delivering `IWindow`
//! methods (oneway) through the target looper. The methods and their order come
//! straight from `android.view.IWindow$Stub` in the boot DEX (`acos-aidl`
//! decodes them, `results/aidl/all_interfaces.json`); the measured launch order
//! is `dispatchAppVisibility` → `windowFocusChanged` → `insetsControlChanged`
//! and the interaction order adds `resized`/`showInsets`/`hideInsets` (see
//! `research/dynamic-trace/data/binder_calls_interaction.tsv`).

use std::collections::VecDeque;

use acos_parcel::binder::BinderId;
use acos_parcel::Parcel;

use crate::codec::{
    write_typed_array, write_typed_object, ClientWindowFrames, InsetsSourceControlArray, InsetsState,
};
use crate::error::Result;
use crate::wms::WindowId;

/// `android.view.IWindow`.
pub const IWINDOW_DESCRIPTOR: &str = "android.view.IWindow";

/// One outgoing callback to a window.
#[derive(Debug, Clone, PartialEq)]
pub enum WmCallback {
    /// `windowFocusChanged(boolean)`. Not present in the API-34 derived
    /// `IWindow` table (focus is folded into `resized`/visibility there); ACOS
    /// keeps it as an explicit, encoded extension at code 17.
    WindowFocusChanged {
        /// Whether the window now has focus.
        focused: bool,
    },
    /// `dispatchAppVisibility(boolean)` (code 7).
    DispatchAppVisibility {
        /// Whether the window is visible.
        visible: bool,
    },
    /// `resized(ClientWindowFrames, boolean, MergedConfiguration, InsetsState,
    /// boolean, boolean, int, int, boolean)` (code 2).
    Resized {
        /// The new window frames.
        frames: Box<ClientWindowFrames>,
        /// `reportDraw`.
        report_draw: bool,
        /// The merged configuration.
        merged_configuration: Box<crate::codec::MergedConfiguration>,
        /// The insets state.
        insets_state: Box<InsetsState>,
        /// `forceLayout`.
        force_layout: bool,
        /// `alwaysConsumeSystemBars`.
        always_consume_system_bars: bool,
        /// `displayId`.
        display_id: i32,
        /// `syncSeqId`.
        sync_seq_id: i32,
        /// `dragResizing`.
        drag_resizing: bool,
    },
    /// `insetsControlChanged(InsetsState, InsetsSourceControl[])` (code 3).
    InsetsControlChanged {
        /// The insets state.
        state: Box<InsetsState>,
        /// The controls.
        controls: Box<InsetsSourceControlArray>,
    },
    /// `showInsets(int, boolean, ImeTracker.Token)` (code 4).
    ShowInsets {
        /// `InternalInsetsType` bits.
        types: i32,
        /// `fromIme`.
        from_ime: bool,
        /// The IME tracker token (written as a nullable typed object).
        token: Option<BinderId>,
    },
    /// `hideInsets(int, boolean, ImeTracker.Token)` (code 5).
    HideInsets {
        /// `InternalInsetsType` bits.
        types: i32,
        /// `fromIme`.
        from_ime: bool,
        /// The IME tracker token.
        token: Option<BinderId>,
    },
    /// `moved(int, int)` (code 6).
    Moved {
        /// New x.
        x: i32,
        /// New y.
        y: i32,
    },
    /// `executeCommand(String, String, ParcelFileDescriptor)` (code 1).
    ExecuteCommand {
        /// `WmCommand` name.
        command: String,
        /// Parameters.
        parameters: String,
    },
    /// `closeSystemDialogs(String)` (code 9).
    CloseSystemDialogs {
        /// The reason string.
        reason: String,
    },
}

impl WmCallback {
    /// The transaction code from the derived `IWindow` table.
    #[must_use]
    pub const fn code(&self) -> u32 {
        match self {
            WmCallback::ExecuteCommand { .. } => 1,
            WmCallback::Resized { .. } => 2,
            WmCallback::InsetsControlChanged { .. } => 3,
            WmCallback::ShowInsets { .. } => 4,
            WmCallback::HideInsets { .. } => 5,
            WmCallback::Moved { .. } => 6,
            WmCallback::DispatchAppVisibility { .. } => 7,
            WmCallback::CloseSystemDialogs { .. } => 9,
            WmCallback::WindowFocusChanged { .. } => 17,
        }
    }

    /// The Java method name.
    #[must_use]
    pub const fn method(&self) -> &'static str {
        match self {
            WmCallback::ExecuteCommand { .. } => "executeCommand",
            WmCallback::Resized { .. } => "resized",
            WmCallback::InsetsControlChanged { .. } => "insetsControlChanged",
            WmCallback::ShowInsets { .. } => "showInsets",
            WmCallback::HideInsets { .. } => "hideInsets",
            WmCallback::Moved { .. } => "moved",
            WmCallback::DispatchAppVisibility { .. } => "dispatchAppVisibility",
            WmCallback::CloseSystemDialogs { .. } => "closeSystemDialogs",
            WmCallback::WindowFocusChanged { .. } => "windowFocusChanged",
        }
    }

    /// `IWindow` methods are all `oneway`.
    #[must_use]
    pub const fn oneway(&self) -> bool {
        true
    }

    /// Write the argument list (no interface token) in the derived order.
    pub fn write_args(&self, p: &mut Parcel) -> Result<()> {
        match self {
            WmCallback::ExecuteCommand {
                command,
                parameters,
            } => {
                p.write_string16(command)?;
                p.write_string16(parameters)?;
                // ParcelFileDescriptor param: null typed object.
                p.write_int32(0);
                Ok(())
            }
            WmCallback::Resized {
                frames,
                report_draw,
                merged_configuration,
                insets_state,
                force_layout,
                always_consume_system_bars,
                display_id,
                sync_seq_id,
                drag_resizing,
            } => {
                write_typed_object(p, Some(frames.as_ref()))?;
                p.write_bool(*report_draw);
                write_typed_object(p, Some(merged_configuration.as_ref()))?;
                write_typed_object(p, Some(insets_state.as_ref()))?;
                p.write_bool(*force_layout);
                p.write_bool(*always_consume_system_bars);
                p.write_int32(*display_id);
                p.write_int32(*sync_seq_id);
                p.write_bool(*drag_resizing);
                Ok(())
            }
            WmCallback::InsetsControlChanged { state, controls } => {
                write_typed_object(p, Some(state.as_ref()))?;
                write_typed_array(p, Some(controls.0.as_slice()))?;
                Ok(())
            }
            WmCallback::ShowInsets {
                types,
                from_ime,
                token,
            }
            | WmCallback::HideInsets {
                types,
                from_ime,
                token,
            } => {
                p.write_int32(*types);
                p.write_bool(*from_ime);
                // ImeTracker.Token is a parcelable; ACOS writes nullptr unless a
                // handle was supplied, in which case the 8-byte id goes inline.
                match token {
                    Some(id) => {
                        p.write_int32(1);
                        p.write_int64(id.0 as i64);
                    }
                    None => p.write_int32(0),
                }
                Ok(())
            }
            WmCallback::Moved { x, y } => {
                p.write_int32(*x);
                p.write_int32(*y);
                Ok(())
            }
            WmCallback::DispatchAppVisibility { visible } => {
                p.write_bool(*visible);
                Ok(())
            }
            WmCallback::CloseSystemDialogs { reason } => {
                p.write_string16(reason)?;
                Ok(())
            }
            WmCallback::WindowFocusChanged { focused } => {
                p.write_bool(*focused);
                Ok(())
            }
        }
    }

    /// A full request parcel: interface token + arguments, as the target's
    /// `$Stub.onTransact` expects. The read position is rewound to the start so
    /// the parcel can be handed straight to `onTransact`.
    pub fn to_parcel(&self) -> Result<Parcel> {
        let mut p = Parcel::new();
        p.write_interface_token(IWINDOW_DESCRIPTOR)?;
        self.write_args(&mut p)?;
        p.set_data_position(0);
        Ok(p)
    }
}

/// A callback addressed to one window.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingCallback {
    /// Target window (`IWindow` binder id).
    pub window: WindowId,
    /// The callback body.
    pub callback: WmCallback,
}

/// Per-window FIFO of outgoing callbacks.
#[derive(Debug, Default)]
pub struct CallbackQueue {
    pending: VecDeque<PendingCallback>,
}

impl CallbackQueue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a callback for `window`.
    pub fn push(&mut self, window: WindowId, callback: WmCallback) {
        self.pending.push_back(PendingCallback { window, callback });
    }

    /// Pop the oldest callback.
    pub fn pop(&mut self) -> Option<PendingCallback> {
        self.pending.pop_front()
    }

    /// Number of queued callbacks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// `true` when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Remove every queued callback (window removed).
    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

/// `true` when `insets` is one of the derived `IWindow` callbacks that the
/// measured traces actually observed (as opposed to the inferred focus one).
#[must_use]
pub fn observed_in_traces(callback: &WmCallback) -> bool {
    matches!(
        callback,
        WmCallback::Resized { .. }
            | WmCallback::InsetsControlChanged { .. }
            | WmCallback::ShowInsets { .. }
            | WmCallback::HideInsets { .. }
            | WmCallback::DispatchAppVisibility { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_delivers_one_bool() {
        let c = WmCallback::DispatchAppVisibility { visible: true };
        let mut p = c.to_parcel().unwrap();
        assert_eq!(c.code(), 7);
        p.enforce_interface(IWINDOW_DESCRIPTOR).unwrap();
        assert!(p.read_bool().unwrap());
        p.enforce_no_data_avail().unwrap();
    }

    #[test]
    fn insets_control_changed_round_trips_frames() {
        let state = InsetsState {
            display_frame: crate::RectI::DISPLAY_1080X2400,
            display_cutout: crate::codec::Cutout::NoCutout,
            rounded_corners: None,
            rounded_corner_frame: crate::RectI::new(0, 0, 0, 0),
            privacy_indicator_bounds: None,
            display_shape: None,
            sources: vec![],
        };
        let c = WmCallback::InsetsControlChanged {
            state: Box::new(state.clone()),
            controls: Box::new(InsetsSourceControlArray(vec![])),
        };
        let mut p = c.to_parcel().unwrap();
        p.enforce_interface(IWINDOW_DESCRIPTOR).unwrap();
        let got = crate::codec::read_typed_object::<InsetsState>(&mut p)
            .unwrap()
            .unwrap();
        assert_eq!(got.display_frame, crate::RectI::DISPLAY_1080X2400);
        let controls = crate::codec::read_typed_array::<crate::codec::InsetsSourceControl>(&mut p)
            .unwrap();
        assert_eq!(controls.unwrap().len(), 0);
    }
}
