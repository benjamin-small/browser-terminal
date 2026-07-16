/**
 * Multi-pane management: reconciles xterm.js instances against engine
 * layout snapshots (Rust owns the tree and the math; this file owns pixels)
 * and funnels input through the sync hot path.
 */
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import type { DividerInfo, Effects, EngineEvent, LayoutSnapshot } from './events.js';

/** Split very large single writes into slices this big. */
const CHUNK_SIZE = 64 * 1024;
/** Enables bracketed paste so multi-line pastes can never auto-execute. */
const ENABLE_BRACKETED_PASTE = '\x1b[?2004h';

const THEME = { background: '#181825' };

/** Write queue chained through term.write callbacks (backpressure). */
export class PaneWriter {
  private queue: string[] = [];
  private writing = false;
  private disposed = false;

  constructor(private readonly term: Terminal) {}

  write(data: string): void {
    if (this.disposed) return;
    for (let i = 0; i < data.length; i += CHUNK_SIZE) {
      this.queue.push(data.slice(i, i + CHUNK_SIZE));
    }
    this.pump();
  }

  dispose(): void {
    this.disposed = true;
    this.queue.length = 0;
  }

  private pump(): void {
    if (this.writing || this.disposed) return;
    const chunk = this.queue.shift();
    if (chunk === undefined) return;
    this.writing = true;
    this.term.write(chunk, () => {
      this.writing = false;
      this.pump();
    });
  }
}

export interface PaneHooks {
  feed(pane: number, data: string): Effects | null;
  resize(pane: number, cols: number, rows: number): void;
  dispatch(msg: Record<string, unknown>): void;
}

interface PaneHandle {
  term: Terminal;
  fit: FitAddon;
  writer: PaneWriter;
  el: HTMLElement;
}

export class PaneManager {
  private panes = new Map<number, PaneHandle>();
  /** Output that arrived before its pane's xterm existed (events precede
   * the layout snapshot that creates panes). */
  private pendingOutput = new Map<number, string[]>();
  private dividerEls: HTMLElement[] = [];
  private activePane = -1;
  private prefixArmed = false;
  private disposed = false;

  constructor(
    private readonly container: HTMLElement,
    private readonly hooks: PaneHooks,
  ) {
    container.style.position = 'relative';
    container.style.overflow = 'hidden';
  }

  get active(): number {
    return this.activePane;
  }

  handleEvent(event: EngineEvent): void {
    if (this.disposed) return;
    switch (event.type) {
      case 'paneOutput': {
        const handle = this.panes.get(event.pane);
        if (handle) {
          handle.writer.write(event.data);
        } else {
          const pending = this.pendingOutput.get(event.pane) ?? [];
          pending.push(event.data);
          this.pendingOutput.set(event.pane, pending);
        }
        break;
      }
      case 'layoutChanged':
        this.applySnapshot(event.snapshot);
        break;
      case 'prefixState':
        this.setPrefixArmed(event.active);
        break;
      case 'paneClosed': {
        // Really gone (kill-pane) — panes merely absent from a snapshot
        // (background sessions/windows) are hidden, keeping their scrollback.
        const handle = this.panes.get(event.pane);
        if (handle) {
          this.destroyPane(event.pane, handle);
        }
        this.pendingOutput.delete(event.pane);
        break;
      }
      case 'paneOpened':
      case 'sessionClosed':
        // The accompanying layoutChanged snapshot is authoritative.
        break;
      case 'fatal':
        for (const handle of this.panes.values()) {
          handle.writer.write(
            `\r\n\x1b[1;31mterminal crashed:\x1b[0m ${event.message}\r\n`,
          );
          handle.term.options.disableStdin = true;
        }
        break;
      case 'hidePanel':
        // Handled by the wrapper (panel visibility).
        break;
    }
  }

  applySnapshot(snapshot: LayoutSnapshot): void {
    const wanted = new Map(snapshot.panes.map((p) => [p.pane, p]));

    // Panes not in this snapshot belong to background windows/sessions:
    // hide them (scrollback survives); paneClosed events destroy for real.
    for (const [id, handle] of this.panes) {
      if (!wanted.has(id)) {
        handle.el.style.display = 'none';
      }
    }

    for (const info of snapshot.panes) {
      const handle = this.panes.get(info.pane) ?? this.createPane(info.pane);
      handle.el.style.display = '';
      const { x, y, w, h } = info.rect;
      handle.el.style.left = `${x * 100}%`;
      handle.el.style.top = `${y * 100}%`;
      handle.el.style.width = `${w * 100}%`;
      handle.el.style.height = `${h * 100}%`;
      handle.el.dataset.active = String(info.pane === snapshot.active_pane);
      handle.el.style.outline =
        info.pane === snapshot.active_pane && snapshot.panes.length > 1
          ? '1px solid #7aa2f7'
          : '1px solid #313244';
    }

    this.activePane = snapshot.active_pane;
    this.renderDividers(snapshot.dividers);

    // Fit after the DOM settles, then hand real cols/rows back to Rust.
    requestAnimationFrame(() => {
      if (this.disposed) return;
      this.fitAll();
      this.panes.get(this.activePane)?.term.focus();
    });
  }

  fitAll(): void {
    for (const [id, handle] of this.panes) {
      if (handle.el.style.display === 'none') continue;
      try {
        handle.fit.fit();
        this.hooks.resize(id, handle.term.cols, handle.term.rows);
      } catch {
        // ignore mid-teardown races
      }
    }
  }

  setPrefixArmed(active: boolean): void {
    this.prefixArmed = active;
    this.container.dataset.prefix = String(active);
    const activeHandle = this.panes.get(this.activePane);
    if (activeHandle) {
      activeHandle.el.style.outline = active
        ? '2px solid #f9e2af'
        : this.panes.size > 1
          ? '1px solid #7aa2f7'
          : '1px solid #313244';
    }
  }

  dispose(): void {
    this.disposed = true;
    for (const [id, handle] of [...this.panes]) {
      this.destroyPane(id, handle);
    }
    for (const el of this.dividerEls) {
      el.remove();
    }
    this.dividerEls = [];
  }

  /** Divider drag: Rust stays authoritative — every pointer move dispatches
   * a resizeSplit and the resulting snapshot re-lays panes out live. */
  private renderDividers(dividers: DividerInfo[]): void {
    for (const el of this.dividerEls) {
      el.remove();
    }
    this.dividerEls = [];
    for (const d of dividers) {
      const el = document.createElement('div');
      const horizontal = d.dir === 'row';
      el.style.cssText = [
        'position:absolute',
        'z-index:10',
        `left:${d.rect.x * 100}%`,
        `top:${d.rect.y * 100}%`,
        horizontal
          ? `width:7px;height:${d.rect.h * 100}%;transform:translateX(-3.5px);cursor:col-resize`
          : `height:7px;width:${d.rect.w * 100}%;transform:translateY(-3.5px);cursor:row-resize`,
      ].join(';');
      el.addEventListener('pointerdown', (ev: PointerEvent) => {
        ev.preventDefault();
        el.setPointerCapture(ev.pointerId);
        let pending: number | null = null;
        const move = (mv: PointerEvent) => {
          const box = this.container.getBoundingClientRect();
          const pointerFrac = horizontal
            ? (mv.clientX - box.left) / box.width
            : (mv.clientY - box.top) / box.height;
          const fraction = (pointerFrac - d.span_start) / d.span_size - d.before;
          if (pending === null) {
            pending = requestAnimationFrame(() => {
              pending = null;
              this.hooks.dispatch({ type: 'resizeSplit', path: d.path, fraction });
            });
          }
        };
        const up = () => {
          el.removeEventListener('pointermove', move);
          el.removeEventListener('pointerup', up);
        };
        el.addEventListener('pointermove', move);
        el.addEventListener('pointerup', up);
      });
      this.container.appendChild(el);
      this.dividerEls.push(el);
    }
  }

  private createPane(id: number): PaneHandle {
    const el = document.createElement('div');
    el.style.cssText = 'position:absolute;box-sizing:border-box;padding:2px;background:#181825;';
    this.container.appendChild(el);

    const term = new Terminal({
      cursorBlink: true,
      scrollback: 2000,
      fontSize: 13,
      theme: THEME,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    term.write(ENABLE_BRACKETED_PASTE);

    // Prefix chord handling: Ctrl-B arms; the next key goes to the keymap.
    term.attachCustomKeyEventHandler((ev: KeyboardEvent) => {
      if (ev.type !== 'keydown') return true;
      if (ev.isComposing || ev.keyCode === 229) return true;
      if (ev.ctrlKey && (ev.key === 'b' || ev.key === 'B') && !this.prefixArmed) {
        this.hooks.dispatch({ type: 'prefixKey' });
        return false;
      }
      if (this.prefixArmed) {
        this.hooks.dispatch({ type: 'key', key: ev.key });
        return false;
      }
      return true;
    });

    term.onData((data) => {
      // Sync hot path: input → WASM editor → echo bytes, same tick.
      const effects = this.hooks.feed(id, data);
      if (effects && effects.echo) {
        term.write(effects.echo);
      }
    });

    el.addEventListener('mousedown', () => {
      if (id !== this.activePane) {
        this.hooks.dispatch({ type: 'focusPane', pane: id });
      }
    });

    const writer = new PaneWriter(term);
    const handle: PaneHandle = { term, fit, writer, el };
    this.panes.set(id, handle);

    const pending = this.pendingOutput.get(id);
    if (pending) {
      this.pendingOutput.delete(id);
      for (const data of pending) {
        writer.write(data);
      }
    }
    return handle;
  }

  private destroyPane(id: number, handle: PaneHandle): void {
    this.panes.delete(id);
    handle.writer.dispose();
    handle.term.dispose();
    handle.el.remove();
  }
}
