/**
 * browser-terminal — public TypeScript API.
 *
 * Current milestone (M3): one floating panel with a full structured-shell
 * REPL pane backed by the Rust/WASM engine. Keystrokes take the sync hot
 * path (`feed` → echo, same tick); engine output arrives through a single
 * event callback and flows through a backpressured write queue.
 */
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import init, { BtermCore } from './wasm/bterm_wasm.js';
import { PaneWriter } from './panes.js';
import type { Effects, EngineEvent } from './events.js';
import type { CommandFn, CommandSpec, Value } from './types.js';

export type { Effects, EngineEvent } from './events.js';
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

/** Enables bracketed paste so multi-line pastes can never auto-execute. */
const ENABLE_BRACKETED_PASTE = '\x1b[?2004h';

export interface CreateOptions {
  /** Element to mount the terminal panel into; a bottom-docked panel is created if omitted. */
  mount?: HTMLElement;
  /** Override the URL of the .wasm binary (for CDN / non-bundler setups). */
  wasmUrl?: string | URL;
}

let instanceLive = false;

export class BrowserTerminal {
  private constructor(
    private readonly core: BtermCore,
    private readonly term: Terminal,
    private readonly writer: PaneWriter,
    private readonly resizeObserver: ResizeObserver,
    private readonly ownedMount: HTMLElement | null,
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

    const term = new Terminal({
      cursorBlink: true,
      scrollback: 2000,
      fontSize: 13,
      theme: { background: '#181825' },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(mount);
    fit.fit();
    term.write(ENABLE_BRACKETED_PASTE);

    const writer = new PaneWriter(term);
    const core = new BtermCore((event: EngineEvent) => {
      switch (event.type) {
        case 'paneOutput':
          writer.write(event.data);
          break;
        case 'fatal':
          writer.write(`\r\n\x1b[1;31mterminal crashed:\x1b[0m ${event.message}\r\n`);
          break;
      }
    });

    core.resize(0, term.cols, term.rows);
    term.onData((data) => {
      // Sync hot path: input → WASM editor → echo bytes, same tick.
      const effects = core.feed(0, data) as Effects | null;
      if (effects && effects.echo) {
        term.write(effects.echo);
      }
    });

    const resizeObserver = new ResizeObserver(() => {
      fit.fit();
      core.resize(0, term.cols, term.rows);
    });
    resizeObserver.observe(mount);

    instanceLive = true;
    return new BrowserTerminal(core, term, writer, resizeObserver, ownedMount);
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
   * Run a line programmatically and get the final structured value —
   * the terminal as a scripting engine for the host page.
   */
  run(line: string): Promise<Value> {
    return this.core.run(0, line) as Promise<Value>;
  }

  dispose(): void {
    this.resizeObserver.disconnect();
    this.writer.dispose();
    this.core.dispose();
    this.core.free();
    this.term.dispose();
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
      'height:320px',
      'background:#181825',
      'padding:8px',
      'border-radius:8px',
      'box-shadow:0 8px 32px rgba(0,0,0,.4)',
      'z-index:2147483000',
    ].join(';');
    document.body.appendChild(el);
    return el;
  }
}
