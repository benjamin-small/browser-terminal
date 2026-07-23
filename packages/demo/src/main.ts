import { BrowserTerminal } from 'browser-terminal';
import { codePanel, helpPanel } from './code';
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
  //
  // Everything below the function is documentation: `summary` and each
  // `desc` are what `links --help` prints. You never register `--help` —
  // the evaluator intercepts it before binding and renders the signature,
  // so the only way to have a badly documented command is to leave these
  // strings out.
  bt.registerCommand(
    {
      name: 'links',
      summary: 'List the links on the host page as a table',
      optional: [{ name: 'pattern', shape: 'str', desc: 'only links whose text contains this' }],
      flags: [
        { long: 'limit', short: 'l', shape: 'int', desc: 'stop after this many links (default 100)' },
        { long: 'internal', short: 'i', desc: 'only links pointing at this origin' },
      ],
    },
    ({ positionals, flags }) => {
      const pattern = String(positionals[0] ?? '').toLowerCase();
      return [...document.querySelectorAll('a')]
        .map((a) => ({ text: a.textContent?.trim() ?? '', href: a.href }))
        .filter((l) => !pattern || l.text.toLowerCase().includes(pattern))
        .filter((l) => !flags.internal || l.href.startsWith(location.origin))
        .slice(0, Number(flags.limit ?? 100));
    },
  );
  // #endregion

  // #region slow
  // Async, cancellable, and progressively printing on two separate channels.
  // `log` is progress, `err` is a warning rendered red — neither enters the
  // pipe, so `slow 5 | length` is unaffected by anything printed. `signal`
  // is a real AbortSignal wired to Ctrl-C, so a hung command can never wedge
  // the pane.
  bt.registerCommand(
    {
      name: 'slow',
      summary: 'Count once a second, printing as it goes (Ctrl-C to abort)',
      optional: [{ name: 'seconds', shape: 'int', desc: 'how long to count for (default 5)' }],
    },
    async ({ positionals }, _input, { signal, log, err }) => {
      const seconds = Number(positionals[0] ?? 5);
      for (let i = 1; i <= seconds; i++) {
        await new Promise((resolve, reject) => {
          const t = setTimeout(resolve, 1000);
          signal.addEventListener('abort', () => {
            clearTimeout(t);
            reject(new DOMException('aborted', 'AbortError'));
          });
        });
        log(`tick ${i}/${seconds}`);
      }
      if (seconds > 3) {
        err(`${seconds}s is a long time to block a pipeline`);
      }
      return `done after ${seconds}s`;
    },
  );
  // #endregion

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
    codePanel('Registering a command over the page DOM', selfSource, 'links'),
    codePanel('An async, cancellable command (slow)', selfSource, 'slow'),
    codePanel('A named selector function (@host)', selfSource, 'selector'),
  );

  // …and what that signature produces. Asking the engine rather than pasting
  // the output means this panel cannot disagree with the code above it.
  const help = document.getElementById('help-panels');
  help?.append(
    helpPanel('links --help', String((await bt.run('links --help')).value)),
    helpPanel('slow --help', String((await bt.run('slow --help')).value)),
  );

  // Expose for programmatic-run experiments in the console.
  (window as unknown as { bt: BrowserTerminal }).bt = bt;
}

void main();
