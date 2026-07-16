/**
 * Floating panel chrome, isolated inside a Shadow DOM root so the host
 * page's CSS can't bleed in (and ours can't bleed out). Header: session
 * name, clickable window tabs, PREFIX cell, minimize/hide. Drag by header,
 * resize by edges, minimize to a session-pill dock strip.
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

  constructor(private readonly hooks: PanelHooks) {
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

    header.append(this.sessionName, this.tabs, spacer, this.prefixCell, minBtn, hideBtn);

    this.content = document.createElement('div');
    this.content.className = 'content';

    this.panel.append(header, this.content);
    this.attachDrag(header);
    this.attachResize();

    this.dock = document.createElement('div');
    this.dock.className = 'dock';

    root.append(this.panel, this.dock);
    document.body.appendChild(this.hostEl);
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

  setPrefix(active: boolean): void {
    this.prefixCell.className = active ? 'prefix-cell on' : 'prefix-cell';
  }

  setMinimized(minimized: boolean): void {
    this.minimized = minimized;
    this.panel.style.display = minimized ? 'none' : 'flex';
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
  }

  dispose(): void {
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
