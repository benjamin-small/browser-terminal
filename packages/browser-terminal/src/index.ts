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
import { PanelHost } from './panels.js';
import type { Effects, EngineEvent, HostMsg, LayoutSnapshot } from './events.js';
import type { CommandFn, CommandSpec, Value } from './types.js';

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
export type {
  CommandArgs,
  CommandCtx,
  CommandFn,
  CommandSpec,
  FlagSpec,
  PosArg,
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
  /** Add a window-level Ctrl+` toggle for the panel (off by default — the
   * library adds no global listeners unless asked). */
  globalToggle?: boolean;
}

let instanceLive = false;

export class BrowserTerminal {
  private lastSnapshot: LayoutSnapshot | null = null;
  private globalToggleHandler: ((ev: KeyboardEvent) => void) | null = null;

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
        opts.wasmUrl ?? new URL('./wasm/bterm_wasm_bg.wasm', import.meta.url),
    });

    let core!: BtermCore;
    let self!: BrowserTerminal;

    let panel: PanelHost | null = null;
    let mount = opts.mount;
    if (!mount) {
      panel = new PanelHost({
        dispatch: (msg: HostMsg) => core.dispatch(msg),
        runCommand: (cmd: string) => {
          (self.run(cmd) as Promise<unknown>).catch(() => {});
        },
      });
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
    this.core.register_command(spec, fn as (...args: unknown[]) => unknown);
  }

  /** Remove a TS-registered command (builtins are not removable). */
  unregisterCommand(name: string): void {
    this.core.unregister_command(name);
  }

  /**
   * Run a line programmatically in the active pane's session and get the
   * final structured value — the terminal as a scripting engine for the
   * host page.
   */
  run(line: string): Promise<Value> {
    const pane = this.lastSnapshot?.active_pane ?? 0;
    return this.core.run(pane, line) as Promise<Value>;
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
}
