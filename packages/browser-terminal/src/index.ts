/**
 * browser-terminal — public TypeScript API.
 *
 * Milestone 1: mounts a single xterm.js terminal and round-trips every
 * keystroke through the Rust/WASM core (`feed` → echo, same tick). The full
 * BrowserTerminal surface (commands, sessions, panels) lands in later
 * milestones.
 */
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import init, { BtermCore } from './wasm/bterm_wasm.js';

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
    const core = new BtermCore();

    let ownedMount: HTMLElement | null = null;
    let mount = opts.mount;
    if (!mount) {
      ownedMount = BrowserTerminal.createDefaultMount();
      mount = ownedMount;
    }

    const term = new Terminal({ cursorBlink: true, scrollback: 2000 });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(mount);
    fit.fit();

    term.write('browser-terminal M1 — echo round-trip through WASM\r\n> ');
    term.onData((data) => {
      // Sync hot path: input → WASM → echo bytes, written in the same tick.
      term.write(core.feed(0, data));
    });

    const resizeObserver = new ResizeObserver(() => fit.fit());
    resizeObserver.observe(mount);

    instanceLive = true;
    return new BrowserTerminal(core, term, resizeObserver, ownedMount);
  }

  dispose(): void {
    this.resizeObserver.disconnect();
    this.term.dispose();
    this.ownedMount?.remove();
    this.core.free();
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
      'background:#1e1e2e',
      'padding:8px',
      'border-radius:8px',
      'box-shadow:0 8px 32px rgba(0,0,0,.4)',
      'z-index:2147483000',
    ].join(';');
    document.body.appendChild(el);
    return el;
  }
}
