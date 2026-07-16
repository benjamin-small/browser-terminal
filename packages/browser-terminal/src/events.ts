/**
 * Engine → host events. Hand-written for now; replaced by generated
 * (tsify-derived) types when the full protocol lands with the TS-command
 * milestone.
 */

export interface PaneOutputEvent {
  type: 'paneOutput';
  pane: number;
  data: string;
}

export interface FatalEvent {
  type: 'fatal';
  message: string;
}

export type EngineEvent = PaneOutputEvent | FatalEvent;

/** Result of feeding input to the engine's sync hot path. */
export interface Effects {
  echo: string;
  submitted: string[];
  ctrl_c: boolean;
}
