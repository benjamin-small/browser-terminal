//! Messages crossing the engine ↔ host boundary. The TS wrapper receives
//! `EngineEvent`s as a tagged union through one callback and sends
//! `HostMsg`s through `dispatch`. Layout crosses as full snapshots — the
//! tree is tiny; no diffing.

use crate::mux::{DividerInfo, Rect};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EngineEvent {
    /// Bytes to write to a pane's terminal (already `\r\n`-converted).
    PaneOutput { pane: u32, data: String },
    /// The pane/window/session arrangement changed; reconcile from the
    /// snapshot.
    LayoutChanged { snapshot: LayoutSnapshot },
    PaneOpened { pane: u32 },
    PaneClosed { pane: u32 },
    SessionClosed { session: u32 },
    /// Prefix armed/disarmed — drive the PREFIX status cell and pane outline.
    PrefixState { active: bool },
    /// `mux hide` (prefix-d): the host should hide the panel.
    HidePanel,
    /// The engine hit a non-recoverable state; the wrapper should disable input.
    Fatal { message: String },
}

/// Host → engine control messages (`dispatch`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HostMsg {
    /// The prefix chord was pressed.
    PrefixKey,
    /// The key following an armed prefix (browser `KeyboardEvent.key`).
    Key { key: String },
    /// The user clicked into a pane.
    FocusPane { pane: u32 },
    /// Status-bar window tab click.
    FocusWindow { window: u32 },
    /// Dock session pill click.
    FocusSession { session: u32 },
    /// Divider drag: set the fraction of the child at `path` in the active
    /// window's split tree (its next sibling absorbs the difference).
    ResizeSplit { path: Vec<usize>, fraction: f32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: u32,
    pub name: String,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: u32,
    pub name: String,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane: u32,
    pub rect: Rect,
    pub active: bool,
}

/// Full picture of the active session (the visible panel renders this) plus
/// the session list for pickers/status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutSnapshot {
    pub sessions: Vec<SessionInfo>,
    /// Windows of the active session.
    pub windows: Vec<WindowInfo>,
    /// Panes of the active window with fractional rects.
    pub panes: Vec<PaneInfo>,
    /// Draggable split boundaries of the active window.
    pub dividers: Vec<DividerInfo>,
    /// The focused pane.
    pub active_pane: u32,
    pub zoomed: Option<u32>,
}
