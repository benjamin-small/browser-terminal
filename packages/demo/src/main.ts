import { BrowserTerminal } from 'browser-terminal';
import { codePanel } from './code';
// The panel below shows this very file — `?raw` guarantees the example on
// the page is the code that actually ran.
import selfSource from './main.ts?raw';

/**
 * The single-file build inlines the wasm and hands it over on `globalThis`
 * before this bundle runs, so nothing is ever fetched. In the normal build
 * this is undefined and the library resolves the .wasm by URL.
 */
const inlinedWasm = (globalThis as { __BTERM_WASM__?: BufferSource }).__BTERM_WASM__;

async function main(): Promise<void> {
  const bt = await BrowserTerminal.create({
    globalToggle: true,
    wasmBinary: inlinedWasm,
  });

  // #region links
  // A page-DOM command in a few lines. Whatever you return is converted to
  // a structured value, so an array of objects renders as a table and can be
  // piped into `filter`, `sort-by`, `grep`, and friends.
  bt.registerCommand(
    {
      name: 'links',
      summary: 'List links on the host page',
      flags: [{ long: 'limit', short: 'l', shape: 'int', desc: 'max links' }],
    },
    ({ flags }) =>
      [...document.querySelectorAll('a')]
        .slice(0, Number(flags.limit ?? 100))
        .map((a) => ({ text: a.textContent?.trim() ?? '', href: a.href })),
  );
  // #endregion

  // Async + cancellable + progressive output, for testing Ctrl-C.
  bt.registerCommand(
    {
      name: 'slow',
      summary: 'Counts for n seconds (Ctrl-C to abort)',
      optional: [{ name: 'seconds', shape: 'int', desc: 'how long' }],
    },
    async ({ positionals }, _input, { signal, emit }) => {
      const seconds = Number(positionals[0] ?? 5);
      for (let i = 1; i <= seconds; i++) {
        await new Promise((resolve, reject) => {
          const t = setTimeout(resolve, 1000);
          signal.addEventListener('abort', () => {
            clearTimeout(t);
            reject(new DOMException('aborted', 'AbortError'));
          });
        });
        emit(`tick ${i}/${seconds}`);
      }
      return `done after ${seconds}s`;
    },
  );

  // A rich-error demo: `fail` shows help in the caret renderer.
  bt.registerCommand(
    { name: 'fail', summary: 'Always throws a rich error' },
    () => {
      throw { message: 'this command always fails', help: 'it exists to demo TS error rendering' };
    },
  );

  // #region selector
  // A named selector function, usable as `@host` anywhere a selector is
  // accepted: `links | map @host`. Needs no eval, so it works under a
  // strict Content-Security-Policy.
  bt.registerFn('host', (item) => {
    const href = (item as { href?: string })?.href ?? '';
    try {
      return new URL(href).hostname;
    } catch {
      return '';
    }
  });
  // #endregion

  // Show the real wiring on the page.
  const host = document.getElementById('source');
  host?.append(
    codePanel('How the `links` command is registered', selfSource, 'links'),
    codePanel('A named selector function (@host)', selfSource, 'selector'),
  );

  // Expose for programmatic-run experiments in the console.
  (window as unknown as { bt: BrowserTerminal }).bt = bt;
}

void main();
