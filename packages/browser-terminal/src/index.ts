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
import { PaneManager, type TerminalOptions } from './panes.js';
import type { ITheme } from '@xterm/xterm';
import { PanelHost, type PanelMode } from './panels.js';
import type { Effects, EngineEvent, HostMsg, LayoutSnapshot } from './events.js';
import { argumentTarget, completionEdit, type CompletionProvider } from './completion.js';
import { BrowserFilesystem } from './filesystem/index.js';
import { createTextEditor } from './filesystem/editor.js';
import { LogFilter, type LogLevel, type Logger } from './log.js';
export type { LogLevel, Logger } from './log.js';
export type { CompletionProvider, CompletionItem, ArgumentCompletionContext } from './completion.js';
import type {
  CommandFn,
  CommandSpec,
  RedirectHandler,
  RunResult,
  SelectorFn,
  Value,
  VarScope,
} from './types.js';

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
export type { TerminalOptions } from './panes.js';
export type { ITheme } from '@xterm/xterm';
export type {
  ChannelWriter,
  CommandArgs,
  CommandCtx,
  CommandFn,
  CommandSpec,
  FlagSpec,
  PosArg,
  RedirectContext,
  RedirectHandler,
  RunError,
  RunResult,
  SelectorFn,
  Shape,
  Value,
  VarScope,
} from './types.js';

export interface CreateOptions {
  /** Mount writable OPFS at /scratch by default. Disable for a host-managed filesystem. */
  filesystem?: boolean;
  /**
   * Element to mount panes into. When provided, the floating panel chrome
   * is skipped — you own the container. Omit for the default draggable
   * bottom panel.
   */
  mount?: HTMLElement;
  /** Theme and font settings applied to every terminal pane. */
  terminal?: TerminalOptions;
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
  /**
   * Which of the library's own console lines to emit (default `'warn'`).
   * A host command error the pane already shows is `'debug'`; library
   * faults are `'error'`. `'silent'` emits nothing. See `setLogLevel()`.
   */
  logLevel?: LogLevel;
  /**
   * Where the library's log lines go (default: the global `console`). It may
   * be called synchronously from inside a running command, so it must not
   * call `dispose()`.
   */
  logger?: Logger;
}

let instanceLive = false;
/** The wasm module instantiates once per page; later `init()` calls return it. */
let wasmLoaded = false;

/**
 * What to hand wasm-bindgen's `init()`. For a URL, fetch it here: when the
 * server sends the wrong MIME type, the generated loader falls back with a
 * bare `console.warn` no log level can reach. Doing that check here routes
 * the warning through the host's filter and passes plain bytes instead.
 */
async function wasmSource(opts: CreateOptions, logs: LogFilter): Promise<BufferSource | Response | undefined> {
  if (opts.wasmBinary) return opts.wasmBinary;
  if (wasmLoaded) return undefined;
  const response = await fetch(opts.wasmUrl ?? new URL('./wasm/bterm_wasm_bg.wasm', import.meta.url));
  if (!response.ok || response.headers.get('Content-Type') === 'application/wasm') return response;
  logs.log(
    'warn',
    'browser-terminal: the server does not serve .wasm as application/wasm; using the slower non-streaming load.',
  );
  return response.arrayBuffer();
}

export class BrowserTerminal {
  private defaultFilesystem: BrowserFilesystem | null = null;
  /** Automatically mounted filesystem, or null when disabled or unavailable. */
  get filesystem(): BrowserFilesystem | null { return this.defaultFilesystem; }
  private lastSnapshot: LayoutSnapshot | null = null;
  private promptProvider: ((context: { session: number; pane: number }) => string) | null = null;
  private readonly completionProviders = new Set<CompletionProvider>();
  private readonly completionRequests = new Map<number, AbortController>();
  private globalToggleHandler: ((ev: KeyboardEvent) => void) | null = null;
  private disposed = false;
  private disposing = false;
  private readonly commandOwners = new Map<string, symbol>();
  private readonly lifecycleListeners = new Set<(event: { type: 'dispose' } | { type: 'sessionClosed'; session: number }) => void>();

  private constructor(
    private readonly logs: LogFilter,
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
    // Before any async work, so a bad level fails fast.
    const logs = new LogFilter(opts.logLevel, opts.logger);
    const source = await wasmSource(opts, logs);
    await init(source === undefined ? undefined : { module_or_path: source });
    wasmLoaded = true;

    let core!: BtermCore;
    let self!: BrowserTerminal;
    let ready = false;

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

    const paneManager = new PaneManager(
      mount,
      {
        feed: (pane, data) => {
          if (!ready) return null;
          self.completionRequests.get(pane)?.abort();
          const effects = core.feed(pane, data) as Effects | null;
          if (effects?.completion) void self.completeArguments(pane, effects.completion);
          return effects;
        },
        resize: (pane, cols, rows) => core.resize(pane, cols, rows),
        dispatch: (msg) => core.dispatch(msg),
      },
      opts.terminal,
    );

    core = new BtermCore((event: EngineEvent) => {
      if (event.type === 'log') {
        logs.log(event.level, event.message);
        return;
      }
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
      if (event.type === 'layoutChanged') self?.refreshPrompt();
      if (event.type === 'sessionClosed') self?.notifyLifecycle(event);
    });

    const resizeObserver = new ResizeObserver(() => paneManager.fitAll());
    resizeObserver.observe(mount);

    instanceLive = true;
    self = new BrowserTerminal(logs, core, paneManager, panel, resizeObserver, mount);

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
    try {
      if (opts.filesystem !== false) await self.initializeFilesystem();
    } catch (error) {
      self.dispose();
      throw error;
    }
    ready = true;
    return self;
  }

  private async initializeFilesystem(): Promise<void> {
    const editor = createTextEditor();
    const filesystem = new BrowserFilesystem({
      initialDirectory: '/scratch',
      editor: editor.open,
      onDirectoryChange: () => this.refreshPrompt(),
    });
    try {
      // Acquire storage before registering commands: unavailable storage leaves
      // the ordinary shell intact, without a partially installed adapter.
      await filesystem.mountScratch('scratch', { writable: true });
    } catch (error) {
      filesystem.dispose();
      editor.dispose();
      const warning = 'browser-terminal: OPFS is unavailable; the default filesystem and file commands were not loaded.';
      this.logs.log('warn', warning, error);
      this.paneManager.handleEvent({
        type: 'paneOutput', pane: this.paneManager.active,
        data: `\r\x1b[K\x1b[33mWarning: ${warning}\x1b[0m\r\n\x1b[32m❯\x1b[0m `,
      });
      return;
    }
    try {
      filesystem.install(this);
      this.setRedirectHandler(filesystem.createRedirectHandler());
      this.defaultFilesystem = filesystem;
      this.setPrompt(({ session }) => `${filesystem.pwd(session)} `);
      this.onLifecycle(event => { if (event.type === 'dispose') editor.dispose(); });
    } catch (error) {
      filesystem.dispose();
      editor.dispose();
      throw error;
    }
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
    this.commandOwners.set(spec.name.trim().replace(/\s+/g, ' '), Symbol());
  }

  /** Register without replacing host commands; cleanup removes only this registration. */
  registerOwnedCommand(spec: CommandSpec, fn: CommandFn): () => void {
    this.assertLive();
    const name = spec.name.trim().replace(/\s+/g, ' ');
    if (this.commandOwners.has(name)) throw new Error(`Command already registered: ${name}`);
    this.registerCommand({ ...spec, name }, fn);
    const owner = this.commandOwners.get(name);
    return () => {
      if (!this.disposed && this.commandOwners.get(name) === owner) this.unregisterCommand(name);
    };
  }

  /** Subscribe to resource cleanup events. Unsubscribe is safe after disposal. */
  onLifecycle(listener: (event: { type: 'dispose' } | { type: 'sessionClosed'; session: number }) => void): () => void {
    this.assertLive();
    this.lifecycleListeners.add(listener);
    return () => { this.lifecycleListeners.delete(listener); };
  }

  private notifyLifecycle(event: { type: 'dispose' } | { type: 'sessionClosed'; session: number }): void {
    for (const listener of [...this.lifecycleListeners]) {
      try { listener(event); } catch (error) { this.logs.log('error', 'Terminal cleanup failed', error); }
    }
  }

  /** Remove a TS-registered command (builtins are not removable). */
  unregisterCommand(name: string): void {
    this.assertLive();
    this.core.unregister_command(name);
    this.commandOwners.delete(name.trim().replace(/\s+/g, ' '));
  }

  /** Enable structured `<`, `>`, and `>>` redirects; null disables future lines. */
  setRedirectHandler(handler: RedirectHandler | null): void {
    this.assertLive();
    if (handler === null) {
      this.core.set_redirect_handler(null);
      return;
    }
    // Getters may reenter or dispose the terminal. Read them before entering
    // WASM, where disposal would also try to free the borrowed core wrapper.
    const read = handler.read;
    const write = handler.write;
    this.assertLive();
    if (typeof read !== 'function' || typeof write !== 'function') {
      throw new Error('redirect handler needs read and write functions');
    }
    this.core.set_redirect_handler({ read: read.bind(handler), write: write.bind(handler) });
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
   * Inject a value the shell resolves as `$name` — how a host page passes
   * its own state to a command without serializing it into the command
   * text or inventing a filename:
   *
   * ```ts
   * bt.setVariable('game', gameDefinition);
   * await bt.run('rtce evaluate --game $game');
   * ```
   *
   * Visible to every session and pane, including ones created later, and
   * inside string interpolation (`"level-$game"`).
   *
   * Takes effect from the next command line: a pipeline already running
   * keeps the values it started with, so a long command cannot see one of
   * its arguments change underneath it.
   *
   * `opts` names the layer: omitted is the engine-wide host one,
   * `{ scope: 'session', session: id }` is one session's, which shadows the
   * host value there. Throws on an id naming no session — a stale id is a
   * bug, and a silent no-op would strand the value nowhere.
   *
   * Throws if `name` is not usable as `$name` (letters, digits and `_`).
   * The value is stored as a typed value and never parsed as shell source.
   */
  setVariable(name: string, value: Value, opts?: VarScope): void {
    this.assertLive();
    this.core.set_variable(name, value, opts);
  }

  /**
   * Replace several variables at once. Every name is validated before any
   * value is applied, so a bad name leaves the previous state untouched —
   * a half-applied batch would leave the shell running against a mix of
   * fresh and stale state.
   *
   * `opts` names the layer exactly as it does for {@link setVariable}, and
   * the whole batch lands in one layer. An unknown session id fails the
   * batch whole, like a bad name does.
   */
  setVariables(values: Record<string, Value>, opts?: VarScope): void {
    this.assertLive();
    this.core.set_variables(values, opts);
  }

  /**
   * Remove an injected variable from the layer `opts` names. Returns
   * whether it was set.
   *
   * The one place an unknown session id is not an error — the other four
   * variable methods throw for one. `boolean` has nowhere to put an error,
   * and a name that is not set in a session that does not exist is,
   * truthfully, not set.
   */
  unsetVariable(name: string, opts?: VarScope): boolean {
    this.assertLive();
    return this.core.unset_variable(name, opts);
  }

  /**
   * The value of an injected variable in the layer `opts` names, or
   * `undefined` if it is not set there.
   *
   * One layer, never a merged view: a session read that fell through to the
   * host value would answer a question you did not ask. For "what would
   * `$name` be here?", the shell's `vars` shows the resolved view.
   *
   * `undefined` rather than `null`, because `null` is itself a legal value
   * to inject — the two have to stay distinguishable.
   *
   * `undefined` means exactly one thing: the name is not set in that layer.
   * Throws if the session id names no session, or this instance has been
   * disposed — both used to read back `undefined` too, leaving a caller
   * unable to tell an absent value from a stale id.
   */
  getVariable(name: string, opts?: VarScope): Value | undefined {
    this.assertLive();
    const v: unknown = this.core.get_variable(name, opts);
    return v === undefined ? undefined : (v as Value);
  }

  /**
   * Everything injected into the layer `opts` names, as a plain object.
   *
   * One layer, never a merged view, so `setVariables(x, opts)` then
   * `variables(opts)` round trips. The shell's `vars` command answers a
   * different question — what `$name` resolves to in a given pane — and
   * shows the merged view.
   *
   * Keys come back sorted. Throws if the session id names no session, or
   * this instance has been disposed — so the declared return type is
   * honest: when this returns, it returns a record.
   */
  variables(opts?: VarScope): Record<string, Value> {
    this.assertLive();
    return this.core.variables(opts) as Record<string, Value>;
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

  /**
   * Replace the theme for all existing and future panes. Omitted colors use
   * terminal defaults; pass an empty object to restore the default theme.
   */
  setTheme(theme: ITheme): void {
    this.assertLive();
    this.paneManager.setTheme(theme);
  }

  /**
   * Set plain text before every pane's status prompt, including future panes.
   * Include spacing, e.g. '/mnt '. Escape sequences and controls are stripped;
   * line breaks and tabs become spaces. Pass '' to restore the default.
   * Idle panes redraw immediately; busy panes update at their next prompt.
   * A callback receives the visible pane/session and is recomputed on layout
   * changes. Call refreshPrompt() when its host state changes.
   */
  setPrompt(prefix: string | ((context: { session: number; pane: number }) => string)): void {
    this.assertLive();
    this.promptProvider = typeof prefix === 'function' ? prefix : null;
    if (typeof prefix === 'string') this.core.set_prompt(prefix);
    else this.refreshPrompt();
  }

  /** The current level for the library's own console output. */
  get logLevel(): LogLevel {
    return this.logs.level;
  }

  /**
   * Change which of the library's console lines are emitted. Throws a
   * RangeError for an unknown level, and an Error after disposal.
   */
  setLogLevel(level: LogLevel): void {
    this.assertLive();
    this.logs.level = level;
  }

  /** Recompute a dynamic prompt after host state changes. Layout changes do this automatically. */
  refreshPrompt(): void {
    this.assertLive();
    for (const request of this.completionRequests.values()) request.abort();
    if (!this.promptProvider || !this.lastSnapshot) return;
    const session = this.lastSnapshot.sessions.find(item => item.active)!.id;
    for (const { pane } of this.lastSnapshot.panes) {
      this.core.set_pane_prompt(pane, this.promptProvider({ session, pane }));
    }
  }

  /** Add argument suggestions. Cleanup removes this provider and cancels pending suggestions. */
  addCompletionProvider(provider: CompletionProvider): () => void {
    this.assertLive();
    this.completionProviders.add(provider);
    return () => {
      this.completionProviders.delete(provider);
      for (const request of this.completionRequests.values()) request.abort();
    };
  }

  private async completeArguments(pane: number, request: NonNullable<Effects['completion']>): Promise<void> {
    const controller = new AbortController();
    this.completionRequests.set(pane, controller);
    try {
      // Let the synchronous key echo settle before an async result redraws it.
      await Promise.resolve();
      if (controller.signal.aborted || this.disposed) return;
      const target = argumentTarget(request.line, this.core.command_specs() as CommandSpec[]);
      if (!target) return;
      const context = { ...target, pane, session: request.session, signal: controller.signal };
      const items = target.flags ? target.flags.map(value => ({ value }))
        : target.shape === 'bool' && target.kind !== 'redirect' ? [{ value: 'true' }, { value: 'false' }]
        : (await Promise.all([...this.completionProviders].map(provider => provider(context)))).flat();
      if (controller.signal.aborted || this.disposed) return;
      const edit = completionEdit(request.line, target.replaceStart, target.prefix, items, !!target.flags || target.shape === 'bool');
      if (edit) this.core.apply_completion(pane, request.revision, request.line, edit.replacement, edit.candidates);
    } catch {
      // Missing directories, revoked permissions, or disconnected mounts leave input intact.
    } finally {
      if (this.completionRequests.get(pane) === controller) this.completionRequests.delete(pane);
    }
  }

  /** Focus the active pane's terminal input. */
  focus(): void {
    this.assertLive();
    this.paneManager.focus();
  }

  /** Remove keyboard focus from the active pane's terminal input. */
  blur(): void {
    this.assertLive();
    this.paneManager.blur();
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
    if (this.disposed || this.disposing) return;
    this.disposing = true;
    for (const request of this.completionRequests.values()) request.abort();
    this.completionRequests.clear();
    this.notifyLifecycle({ type: 'dispose' });
    this.lifecycleListeners.clear();
    this.commandOwners.clear();
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
