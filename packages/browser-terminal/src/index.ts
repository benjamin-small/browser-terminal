/**
 * browser-terminal — public TypeScript API.
 *
 * A floating panel hosting a tmux-style multiplexer: the Rust/WASM engine
 * owns sessions, windows, the layout tree, the shell language, and the
 * command registry; this wrapper owns pixels (xterm panes positioned from
 * fractional snapshots), the prefix chord, and the panel chrome (Shadow
 * DOM: drag, resize, tabs, session pills).
 */
import init, { BtermCore } from './wasm/bterm_wasm.js';
import { PaneManager } from './panes.js';
import { PanelHost, type PanelMode } from './panels.js';
import type { Effects, EngineEvent, HostMsg, LayoutSnapshot } from './events.js';
import type { CommandFn, CommandSpec, RunResult, SelectorFn } from './types.js';

export type {
  DividerInfo,
  Effects,
  EngineEvent,
  HostMsg,
  LayoutSnapshot,
  PaneInfo,
  Rect,
  SessionInfo,
  WindowInfo,
} from './events.js';
export type { PanelMode } from './panels.js';
export type {
  CommandArgs,
  CommandCtx,
  CommandFn,
  CommandSpec,
  FlagSpec,
  PosArg,
  RunError,
  RunResult,
  SelectorFn,
  Shape,
  Value,
} from './types.js';

export interface CreateOptions {
  /**
   * Element to mount panes into. When provided, the floating panel chrome
   * is skipped — you own the container. Omit for the default draggable
   * bottom panel.
   */
  mount?: HTMLElement;
  /** Override the URL of the .wasm binary (for CDN / non-bundler setups). */
  wasmUrl?: string | URL;
  /**
   * Pre-loaded wasm bytes, used instead of fetching. Takes precedence over
   * `wasmUrl`. This is what makes a single-file build possible: `file://`
   * pages can't `fetch()` anything, but they can decode an inlined binary.
   */
  wasmBinary?: BufferSource;
  /** Add a window-level Ctrl+` toggle for the panel (off by default — the
   * library adds no global listeners unless asked). */
  globalToggle?: boolean;
  /**
   * How the panel sits on the page. `'right'` (default) docks it full-height
   * to that edge and reflows the page's content beside it; `'float'` gives
   * the draggable window. Switchable at runtime via the header button or
   * `setPanelMode()`.
   */
  dock?: PanelMode;
  /** Docked width in px (default 480); drag the inner edge to change it. */
  dockWidth?: number;
  /** Element padded to make room when docked. Defaults to `document.body`. */
  dockTarget?: HTMLElement;
}

let instanceLive = false;

export class BrowserTerminal {
  private lastSnapshot: LayoutSnapshot | null = null;
  private globalToggleHandler: ((ev: KeyboardEvent) => void) | null = null;
  private disposed = false;

  private constructor(
    private readonly core: BtermCore,
    private readonly paneManager: PaneManager,
    private readonly panel: PanelHost | null,
    private readonly resizeObserver: ResizeObserver,
    private readonly mount: HTMLElement,
  ) {}

  static async create(opts: CreateOptions = {}): Promise<BrowserTerminal> {
    if (instanceLive) {
      throw new Error(
        'browser-terminal: one instance per page in v1; call dispose() first.',
      );
    }
    await init({
      module_or_path:
        opts.wasmBinary ??
        opts.wasmUrl ??
        new URL('./wasm/bterm_wasm_bg.wasm', import.meta.url),
    });

    let core!: BtermCore;
    let self!: BrowserTerminal;

    let panel: PanelHost | null = null;
    let mount = opts.mount;
    if (!mount) {
      panel = new PanelHost(
        {
          dispatch: (msg: HostMsg) => core.dispatch(msg),
          runCommand: (cmd: string) => {
            (self.run(cmd) as Promise<unknown>).catch(() => {});
          },
          // The panel's box changed; xterm must re-measure or the grid
          // keeps the old column count.
          resized: () => paneManager.fitAll(),
        },
        { mode: opts.dock, width: opts.dockWidth, dockTarget: opts.dockTarget },
      );
      mount = panel.contentEl;
    }

    const paneManager = new PaneManager(mount, {
      feed: (pane, data) => core.feed(pane, data) as Effects | null,
      resize: (pane, cols, rows) => core.resize(pane, cols, rows),
      dispatch: (msg) => core.dispatch(msg),
    });

    core = new BtermCore((event: EngineEvent) => {
      switch (event.type) {
        case 'layoutChanged':
          self.lastSnapshot = event.snapshot;
          panel?.applySnapshot(event.snapshot);
          break;
        case 'prefixState':
          panel?.setPrefix(event.active);
          break;
        case 'hidePanel':
          self.hide();
          break;
      }
      paneManager.handleEvent(event);
    });

    const resizeObserver = new ResizeObserver(() => paneManager.fitAll());
    resizeObserver.observe(mount);

    instanceLive = true;
    self = new BrowserTerminal(core, paneManager, panel, resizeObserver, mount);

    if (opts.globalToggle) {
      self.globalToggleHandler = (ev: KeyboardEvent) => {
        if (ev.ctrlKey && ev.key === '`') {
          ev.preventDefault();
          self.toggle();
        }
      };
      window.addEventListener('keydown', self.globalToggleHandler);
    }

    // The constructor emitted banner/prompt/layout before `self` existed;
    // reconcile from a fresh snapshot.
    const snapshot = core.snapshot() as LayoutSnapshot | null;
    if (snapshot) {
      self.lastSnapshot = snapshot;
      panel?.applySnapshot(snapshot);
      paneManager.applySnapshot(snapshot);
    }
    return self;
  }

  /**
   * Register a shell command implemented in TypeScript. The function
   * receives `({ positionals, flags }, input, { signal, emit })`; plain
   * return values auto-convert (arrays of objects render as tables).
   * Throws if the name collides with a builtin; re-registering a TS command
   * replaces it (hot-reload friendly).
   */
  registerCommand(spec: CommandSpec, fn: CommandFn): void {
    this.assertLive();
    this.core.register_command(spec, fn as (...args: unknown[]) => unknown);
  }

  /** Remove a TS-registered command (builtins are not removable). */
  unregisterCommand(name: string): void {
    this.assertLive();
    this.core.unregister_command(name);
  }

  /**
   * Register a function usable as `@name` wherever a selector is accepted:
   * `map @slug`, `filter @isActive`, `grep pattern --on @key`.
   *
   * This is the CSP-safe counterpart to inline `'(o) => …'` source — no
   * `eval`, so it works on pages with a strict Content-Security-Policy, and
   * the function stays type-checked and breakpoint-able.
   */
  registerFn(name: string, fn: SelectorFn): void {
    this.assertLive();
    this.core.register_fn(name, fn as (...args: unknown[]) => unknown);
  }

  /** Remove a registered selector function. */
  unregisterFn(name: string): void {
    this.assertLive();
    this.core.unregister_fn(name);
  }

  /**
   * Run a line programmatically in the active pane's session and get back
   * `{ value, log, err }` — the terminal as a scripting engine for the
   * host page. Diagnostics (`ctx.log` / `ctx.err`) come back as arrays
   * instead of printing, so a background call (e.g. from a `useEffect`)
   * never writes on whatever pane happens to be active; the caller decides
   * what, if anything, to surface.
   *
   * Rejects with a {@link RunError} on failure or Ctrl-C — an ordinary
   * `Error` that also carries the `log` and `err` written before things
   * went wrong, which is when they are most worth having.
   */
  run(line: string): Promise<RunResult> {
    if (this.disposed) {
      return Promise.reject(new Error('browser-terminal: instance is disposed'));
    }
    const pane = this.lastSnapshot?.active_pane ?? 0;
    return this.core.run(pane, line) as Promise<RunResult>;
  }

  /**
   * Dock the panel to an edge (page content reflows beside it) or pop it
   * out into a floating window. No-op when you supplied your own `mount`.
   */
  setPanelMode(mode: PanelMode): void {
    this.panel?.setMode(mode);
  }

  /** Current panel mode, or `null` with a custom `mount`. */
  get panelMode(): PanelMode | null {
    return this.panel?.panelMode ?? null;
  }

  /** The latest layout snapshot (sessions, windows, pane rects). */
  get snapshot(): LayoutSnapshot | null {
    return this.lastSnapshot;
  }

  show(): void {
    if (this.panel) {
      this.panel.show();
    } else {
      this.mount.style.display = '';
    }
    this.paneManager.fitAll();
  }

  hide(): void {
    if (this.panel) {
      this.panel.hide();
    } else {
      this.mount.style.display = 'none';
    }
  }

  toggle(): void {
    const hidden = this.panel
      ? this.panel.hostEl.style.display === 'none'
      : this.mount.style.display === 'none';
    if (hidden) {
      this.show();
    } else {
      this.hide();
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    if (this.globalToggleHandler) {
      window.removeEventListener('keydown', this.globalToggleHandler);
    }
    this.resizeObserver.disconnect();
    this.paneManager.dispose();
    this.core.dispose();
    this.core.free();
    this.panel?.dispose();
    instanceLive = false;
  }

  private assertLive(): void {
    if (this.disposed) {
      throw new Error('browser-terminal: instance is disposed');
    }
  }
}
