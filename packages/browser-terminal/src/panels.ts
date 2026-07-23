/**
 * Panel chrome, isolated inside a Shadow DOM root so the host page's CSS
 * can't bleed in (and ours can't bleed out).
 *
 * Two modes:
 *
 * - **docked** (default) — pinned to a screen edge, full height, with the
 *   host page's content reflowed beside it rather than hidden underneath.
 *   The page keeps its own stacking context, so the terminal reads as part
 *   of the layout instead of an overlay covering it.
 * - **floating** — the draggable, edge-resizable window, for when you want
 *   the terminal over the content rather than beside it.
 *
 * Space is reserved by padding the dock target (the body by default), so no
 * cooperation is needed from the host page's own layout. The original
 * padding is captured once and restored on float/hide/dispose.
 */
import { XTERM_CSS } from './generated/xterm-css.js';
import type { HostMsg, LayoutSnapshot } from './events.js';

const PANEL_CSS = `
:host { all: initial; }
* { box-sizing: border-box; }
.panel {
  position: fixed;
  left: 50%;
  bottom: 16px;
  transition: none;
  width: min(920px, calc(100vw - 48px));
  height: 400px;
  min-width: 360px;
  min-height: 220px;
  margin-left: calc(-1 * min(920px, calc(100vw - 48px)) / 2);
  background: #181825;
  border: 1px solid #313244;
  border-radius: 10px;
  box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
  display: flex;
  flex-direction: column;
  overflow: hidden;
  z-index: 2147483000;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
  color: #cdd6f4;
}
.header {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 10px;
  background: #11111b;
  border-bottom: 1px solid #313244;
  cursor: grab;
  user-select: none;
  flex: none;
}
.header:active { cursor: grabbing; }
.session-name { color: #a6e3a1; font-weight: 600; }
.tabs { display: flex; gap: 4px; align-items: center; }
.tab {
  padding: 2px 8px;
  border-radius: 4px;
  background: transparent;
  color: #9399b2;
  cursor: pointer;
  border: none;
  font: inherit;
}
.tab:hover { background: #313244; color: #cdd6f4; }
.tab.active { background: #313244; color: #cdd6f4; }
.tab.add { color: #6c7086; }
.spacer { flex: 1; }
.prefix-cell {
  padding: 2px 8px;
  border-radius: 4px;
  background: #f9e2af;
  color: #11111b;
  font-weight: 700;
  display: none;
}
.prefix-cell.on { display: inline-block; }
.btn {
  border: none;
  background: transparent;
  color: #9399b2;
  cursor: pointer;
  font: inherit;
  padding: 2px 6px;
  border-radius: 4px;
}
.btn:hover { background: #313244; color: #cdd6f4; }
.content { position: relative; flex: 1; min-height: 0; }
.edge { position: absolute; z-index: 5; }
.edge.right { right: -3px; top: 0; bottom: 0; width: 6px; cursor: ew-resize; }
.edge.bottom { bottom: -3px; left: 0; right: 0; height: 6px; cursor: ns-resize; }
.edge.corner { right: -4px; bottom: -4px; width: 12px; height: 12px; cursor: nwse-resize; }
/* Docked: pinned full-height to an edge, square inner corner, no drag. */
.panel.dock-right,
.panel.dock-left {
  top: 0;
  bottom: 0;
  height: auto;
  left: auto;
  right: 0;
  margin-left: 0;
  border-radius: 0;
  border-top: none;
  border-bottom: none;
  border-right: none;
  box-shadow: -10px 0 30px rgba(0, 0, 0, 0.35);
  transform: none !important;
}
.panel.dock-left {
  right: auto;
  left: 0;
  border-right: 1px solid #313244;
  border-left: none;
  box-shadow: 10px 0 30px rgba(0, 0, 0, 0.35);
}
.panel.dock-right .header,
.panel.dock-left .header {
  cursor: default;
}
/* Drag handle for the dock's inner edge. */
.dock-resizer {
  position: absolute;
  top: 0;
  bottom: 0;
  width: 7px;
  cursor: ew-resize;
  z-index: 6;
}
.panel.dock-right .dock-resizer { left: -3px; }
.panel.dock-left .dock-resizer { right: -3px; }
.panel:not(.dock-right):not(.dock-left) .dock-resizer { display: none; }
/* Edge handles and dragging only make sense while floating. */
.panel.dock-right .edge,
.panel.dock-left .edge { display: none; }

.dock {
  position: fixed;
  left: 50%;
  transform: translateX(-50%);
  bottom: 2px;
  display: none;
  gap: 6px;
  z-index: 2147483001;
}
.dock.visible { display: flex; }
.pill {
  padding: 4px 12px;
  border-radius: 999px;
  border: 1px solid #313244;
  background: #181825;
  color: #cdd6f4;
  cursor: pointer;
  font: inherit;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.4);
}
.pill.active { border-color: #a6e3a1; }
.pill:hover { background: #313244; }
`;

export interface PanelHooks {
  dispatch(msg: HostMsg): void;
  runCommand(cmd: string): void;
  /** Panes must re-fit whenever the panel's box changes. */
  resized(): void;
}

/** `right`/`left` dock to that edge; `float` is the draggable window. */
export type PanelMode = 'right' | 'left' | 'float';

export interface PanelOptions {
  mode?: PanelMode;
  /** Docked width in px (drag the inner edge to change it). */
  width?: number;
  /**
   * Element padded to make room for a docked panel. Defaults to the body,
   * which is right for ordinary document flow; pass your own scroll
   * container if the app manages its own layout.
   */
  dockTarget?: HTMLElement;
}

export class PanelHost {
  readonly hostEl: HTMLElement;
  private readonly panel: HTMLElement;
  private readonly content: HTMLElement;
  private readonly sessionName: HTMLElement;
  private readonly tabs: HTMLElement;
  private readonly prefixCell: HTMLElement;
  private readonly dock: HTMLElement;
  private minimized = false;
  private lastSnapshot: LayoutSnapshot | null = null;
  private mode: PanelMode;
  private dockWidth: number;
  private readonly dockTarget: HTMLElement;
  /** Captured once so float/hide/dispose can put the page back exactly. */
  private readonly originalPadding: { right: string; left: string; transition: string };
  private readonly modeBtn: HTMLButtonElement;

  constructor(
    private readonly hooks: PanelHooks,
    opts: PanelOptions = {},
  ) {
    this.mode = opts.mode ?? 'right';
    this.dockWidth = opts.width ?? 480;
    this.dockTarget = opts.dockTarget ?? document.body;
    this.originalPadding = {
      right: this.dockTarget.style.paddingRight,
      left: this.dockTarget.style.paddingLeft,
      transition: this.dockTarget.style.transition,
    };
    this.hostEl = document.createElement('div');
    this.hostEl.setAttribute('data-browser-terminal', '');
    const root = this.hostEl.attachShadow({ mode: 'open' });

    const style = document.createElement('style');
    style.textContent = XTERM_CSS + PANEL_CSS;
    root.appendChild(style);

    this.panel = document.createElement('div');
    this.panel.className = 'panel';

    const header = document.createElement('div');
    header.className = 'header';
    this.sessionName = document.createElement('span');
    this.sessionName.className = 'session-name';
    this.tabs = document.createElement('span');
    this.tabs.className = 'tabs';
    const spacer = document.createElement('span');
    spacer.className = 'spacer';
    this.prefixCell = document.createElement('span');
    this.prefixCell.className = 'prefix-cell';
    this.prefixCell.textContent = 'PREFIX';

    this.modeBtn = document.createElement('button');
    this.modeBtn.className = 'btn';
    this.modeBtn.addEventListener('click', () =>
      this.setMode(this.mode === 'float' ? 'right' : 'float'),
    );

    const minBtn = document.createElement('button');
    minBtn.className = 'btn';
    minBtn.title = 'Minimize';
    minBtn.textContent = '─';
    minBtn.addEventListener('click', () => this.setMinimized(true));

    const hideBtn = document.createElement('button');
    hideBtn.className = 'btn';
    hideBtn.title = 'Hide (bt.show() restores)';
    hideBtn.textContent = '✕';
    hideBtn.addEventListener('click', () => this.hide());

    header.append(this.sessionName, this.tabs, spacer, this.prefixCell, this.modeBtn, minBtn, hideBtn);

    this.content = document.createElement('div');
    this.content.className = 'content';

    const dockResizer = document.createElement('div');
    dockResizer.className = 'dock-resizer';
    this.panel.append(header, this.content, dockResizer);
    this.attachDrag(header);
    this.attachResize();
    this.attachDockResize(dockResizer);

    this.dock = document.createElement('div');
    this.dock.className = 'dock';

    root.append(this.panel, this.dock);
    document.body.appendChild(this.hostEl);
    this.applyMode();
  }

  /** Where the PaneManager mounts. */
  get contentEl(): HTMLElement {
    return this.content;
  }

  applySnapshot(snapshot: LayoutSnapshot): void {
    this.lastSnapshot = snapshot;
    const active = snapshot.sessions.find((s) => s.active);
    this.sessionName.textContent = active ? `[${active.name}]` : '';

    this.tabs.replaceChildren(
      ...snapshot.windows.map((w, i) => {
        const tab = document.createElement('button');
        tab.className = w.active ? 'tab active' : 'tab';
        tab.textContent = `${i + 1}:${w.name}${w.active ? '*' : ''}`;
        tab.addEventListener('click', () =>
          this.hooks.dispatch({ type: 'focusWindow', window: w.id }),
        );
        return tab;
      }),
      (() => {
        const add = document.createElement('button');
        add.className = 'tab add';
        add.textContent = '+';
        add.title = 'New window (Ctrl-B c)';
        add.addEventListener('click', () => this.hooks.runCommand('mux window new'));
        return add;
      })(),
    );

    this.renderDock();
  }

  /** Switch between docked and floating. */
  setMode(mode: PanelMode): void {
    this.mode = mode;
    if (mode !== 'float') {
      // A previous drag left a transform behind; docking must sit flush.
      this.panel.style.transform = '';
    }
    this.applyMode();
    this.hooks.resized();
  }

  get panelMode(): PanelMode {
    return this.mode;
  }

  private applyMode(): void {
    this.panel.classList.toggle('dock-right', this.mode === 'right');
    this.panel.classList.toggle('dock-left', this.mode === 'left');
    if (this.mode === 'float') {
      this.panel.style.width = '';
      this.panel.style.height = '';
      this.panel.style.marginLeft = '';
      this.modeBtn.textContent = '⇥';
      this.modeBtn.title = 'Dock to the right edge';
    } else {
      this.panel.style.width = `${this.dockWidth}px`;
      this.modeBtn.textContent = '⇱';
      this.modeBtn.title = 'Pop out into a floating window';
    }
    this.reserveSpace();
  }

  /**
   * Reserve room for a docked panel by padding the dock target, so page
   * content reflows beside the terminal instead of hiding under it. Hidden
   * or minimized panels reserve nothing.
   */
  private reserveSpace(): void {
    const hidden = this.hostEl.style.display === 'none' || this.minimized;
    const active = !hidden && this.mode !== 'float';
    // Transition only the property we drive, and only while docked, so the
    // host page's own transitions are left alone.
    this.dockTarget.style.transition = active ? 'padding 120ms ease' : this.originalPadding.transition;
    this.dockTarget.style.paddingRight =
      active && this.mode === 'right' ? `${this.dockWidth}px` : this.originalPadding.right;
    this.dockTarget.style.paddingLeft =
      active && this.mode === 'left' ? `${this.dockWidth}px` : this.originalPadding.left;
  }

  /** Drag the dock's inner edge to resize; the page reflows live. */
  private attachDockResize(handle: HTMLElement): void {
    handle.addEventListener('pointerdown', (ev: PointerEvent) => {
      if (this.mode === 'float') return;
      ev.preventDefault();
      handle.setPointerCapture(ev.pointerId);
      const startX = ev.clientX;
      const startWidth = this.dockWidth;
      const move = (mv: PointerEvent) => {
        const delta = this.mode === 'right' ? startX - mv.clientX : mv.clientX - startX;
        // Leave at least a third of the viewport for the page itself.
        const max = Math.max(320, window.innerWidth * 0.66);
        this.dockWidth = Math.min(max, Math.max(280, startWidth + delta));
        this.panel.style.width = `${this.dockWidth}px`;
        this.reserveSpace();
        this.hooks.resized();
      };
      const up = () => {
        handle.removeEventListener('pointermove', move);
        handle.removeEventListener('pointerup', up);
      };
      handle.addEventListener('pointermove', move);
      handle.addEventListener('pointerup', up);
    });
  }

  setPrefix(active: boolean): void {
    this.prefixCell.className = active ? 'prefix-cell on' : 'prefix-cell';
  }

  setMinimized(minimized: boolean): void {
    this.minimized = minimized;
    this.panel.style.display = minimized ? 'none' : 'flex';
    this.reserveSpace();
    this.renderDock();
  }

  get isMinimized(): boolean {
    return this.minimized;
  }

  show(): void {
    this.hostEl.style.display = '';
    this.setMinimized(false);
  }

  hide(): void {
    this.hostEl.style.display = 'none';
    this.reserveSpace();
  }

  dispose(): void {
    // Put the host page's layout back exactly as we found it.
    this.dockTarget.style.paddingRight = this.originalPadding.right;
    this.dockTarget.style.paddingLeft = this.originalPadding.left;
    this.dockTarget.style.transition = this.originalPadding.transition;
    this.hostEl.remove();
  }

  private renderDock(): void {
    const snapshot = this.lastSnapshot;
    const showPills = this.minimized || (snapshot?.sessions.length ?? 0) > 1;
    this.dock.className = showPills ? 'dock visible' : 'dock';
    if (!snapshot || !showPills) {
      this.dock.replaceChildren();
      return;
    }
    this.dock.replaceChildren(
      ...snapshot.sessions.map((s) => {
        const pill = document.createElement('button');
        pill.className = s.active && !this.minimized ? 'pill active' : 'pill';
        pill.textContent = s.name;
        pill.title = `Switch to session ${s.name}`;
        pill.addEventListener('click', () => {
          this.setMinimized(false);
          this.hooks.dispatch({ type: 'focusSession', session: s.id });
        });
        return pill;
      }),
    );
  }

  private attachDrag(header: HTMLElement): void {
    let startX = 0;
    let startY = 0;
    let baseX = 0;
    let baseY = 0;
    header.addEventListener('pointerdown', (ev: PointerEvent) => {
      if (this.mode !== 'float') return;
      if ((ev.target as HTMLElement).tagName === 'BUTTON') return;
      startX = ev.clientX;
      startY = ev.clientY;
      const move = (mv: PointerEvent) => {
        const dx = baseX + (mv.clientX - startX);
        const dy = baseY + (mv.clientY - startY);
        this.panel.style.transform = `translate(${dx}px, ${dy}px)`;
      };
      const up = (uv: PointerEvent) => {
        baseX += uv.clientX - startX;
        baseY += uv.clientY - startY;
        window.removeEventListener('pointermove', move);
        window.removeEventListener('pointerup', up);
      };
      window.addEventListener('pointermove', move);
      window.addEventListener('pointerup', up);
    });
  }

  private attachResize(): void {
    const make = (cls: string, horizontal: boolean, vertical: boolean) => {
      const edge = document.createElement('div');
      edge.className = `edge ${cls}`;
      edge.addEventListener('pointerdown', (ev: PointerEvent) => {
        ev.preventDefault();
        const startX = ev.clientX;
        const startY = ev.clientY;
        const rect = this.panel.getBoundingClientRect();
        const move = (mv: PointerEvent) => {
          if (horizontal) {
            this.panel.style.width = `${Math.max(360, rect.width + (mv.clientX - startX))}px`;
            this.panel.style.marginLeft = `${-(rect.width + (mv.clientX - startX)) / 2}px`;
          }
          if (vertical) {
            this.panel.style.height = `${Math.max(220, rect.height + (mv.clientY - startY))}px`;
          }
        };
        const up = () => {
          window.removeEventListener('pointermove', move);
          window.removeEventListener('pointerup', up);
        };
        window.addEventListener('pointermove', move);
        window.addEventListener('pointerup', up);
      });
      this.panel.appendChild(edge);
    };
    make('right', true, false);
    make('bottom', false, true);
    make('corner', true, true);
  }
}
