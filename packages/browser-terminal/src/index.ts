/**
 * browser-terminal — public TypeScript API.
 *
 * A floating panel hosting a tmux-style multiplexer: the Rust/WASM engine
 * owns sessions, windows, the layout tree, the shell language, and the
 * command registry; this wrapper owns pixels (xterm panes positioned from
 * fractional snapshots) and the prefix chord.
 */
import init, { BtermCore } from './wasm/bterm_wasm.js';
import { PaneManager } from './panes.js';
import type { Effects, EngineEvent, LayoutSnapshot } from './events.js';
import type { CommandFn, CommandSpec, Value } from './types.js';

export type {
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
  /** Element to mount the terminal panel into; a bottom-docked panel is created if omitted. */
  mount?: HTMLElement;
  /** Override the URL of the .wasm binary (for CDN / non-bundler setups). */
  wasmUrl?: string | URL;
}

let instanceLive = false;

export class BrowserTerminal {
  private lastSnapshot: LayoutSnapshot | null = null;

  private constructor(
    private readonly core: BtermCore,
    private readonly paneManager: PaneManager,
    private readonly resizeObserver: ResizeObserver,
    private readonly ownedMount: HTMLElement | null,
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

    let ownedMount: HTMLElement | null = null;
    let mount = opts.mount;
    if (!mount) {
      ownedMount = BrowserTerminal.createDefaultMount();
      mount = ownedMount;
    }

    let core!: BtermCore;
    let self!: BrowserTerminal;
    const paneManager = new PaneManager(mount, {
      feed: (pane, data) => core.feed(pane, data) as Effects | null,
      resize: (pane, cols, rows) => core.resize(pane, cols, rows),
      dispatch: (msg) => core.dispatch(msg),
    });

    core = new BtermCore((event: EngineEvent) => {
      if (event.type === 'layoutChanged') {
        self.lastSnapshot = event.snapshot;
      }
      if (event.type === 'hidePanel') {
        self.hide();
      }
      paneManager.handleEvent(event);
    });

    const resizeObserver = new ResizeObserver(() => paneManager.fitAll());
    resizeObserver.observe(mount);

    instanceLive = true;
    self = new BrowserTerminal(core, paneManager, resizeObserver, ownedMount, mount);
    // The constructor emitted banner/prompt/layout before `self` existed;
    // reconcile from a fresh snapshot.
    const snapshot = core.snapshot() as LayoutSnapshot | null;
    if (snapshot) {
      self.lastSnapshot = snapshot;
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
    this.mount.style.display = '';
    this.paneManager.fitAll();
  }

  hide(): void {
    this.mount.style.display = 'none';
  }

  toggle(): void {
    if (this.mount.style.display === 'none') {
      this.show();
    } else {
      this.hide();
    }
  }

  dispose(): void {
    this.resizeObserver.disconnect();
    this.paneManager.dispose();
    this.core.dispose();
    this.core.free();
    this.ownedMount?.remove();
    instanceLive = false;
  }

  private static createDefaultMount(): HTMLElement {
    const el = document.createElement('div');
    el.style.cssText = [
      'position:fixed',
      'left:16px',
      'right:16px',
      'bottom:16px',
      'height:340px',
      'background:#181825',
      'padding:6px',
      'border-radius:8px',
      'box-shadow:0 8px 32px rgba(0,0,0,.4)',
      'z-index:2147483000',
    ].join(';');
    document.body.appendChild(el);
    return el;
  }
}
