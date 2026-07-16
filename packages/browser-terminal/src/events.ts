/**
 * Engine ↔ host protocol types. These mirror `bterm-core/src/protocol.rs`
 * exactly (tagged unions, camelCase tags).
 */

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface SessionInfo {
  id: number;
  name: string;
  active: boolean;
}

export interface WindowInfo {
  id: number;
  name: string;
  active: boolean;
}

export interface PaneInfo {
  pane: number;
  rect: Rect;
  active: boolean;
}

export interface LayoutSnapshot {
  sessions: SessionInfo[];
  windows: WindowInfo[];
  panes: PaneInfo[];
  active_pane: number;
  zoomed: number | null;
}

export type EngineEvent =
  | { type: 'paneOutput'; pane: number; data: string }
  | { type: 'layoutChanged'; snapshot: LayoutSnapshot }
  | { type: 'paneOpened'; pane: number }
  | { type: 'paneClosed'; pane: number }
  | { type: 'sessionClosed'; session: number }
  | { type: 'prefixState'; active: boolean }
  | { type: 'hidePanel' }
  | { type: 'fatal'; message: string };

export type HostMsg =
  | { type: 'prefixKey' }
  | { type: 'key'; key: string }
  | { type: 'focusPane'; pane: number }
  | { type: 'resizeSplit'; path: number[]; fraction: number };

/** Result of feeding input to the engine's sync hot path. */
export interface Effects {
  echo: string;
  submitted: string[];
  ctrl_c: boolean;
}
