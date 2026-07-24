/**
 * Smoke suite (milestone 7): proves the wired-together system in a real
 * browser — engine boot, TS command registration, the flagship pipeline,
 * prefix-key splits, session pills, and dispose.
 */
import { expect, test } from '@playwright/test';

declare global {
  interface Window {
    bt: {
      run(line: string): Promise<{ value: unknown; log: string[]; err: string[] }>;
      dispose(): void;
      registerCommand(
        spec: { name: string; summary?: string },
        fn: (
          args: unknown,
          input: unknown,
          ctx: { log(s: string): void; err(s: string): void; emit(s: string): void },
        ) => unknown,
      ): void;
    };
  }
}

function shadow(selector: string) {
  return `document.querySelector('[data-browser-terminal]').shadowRoot.querySelector('${selector}')`;
}

async function waitForTerminal(page: import('@playwright/test').Page) {
  await page.waitForFunction(
    () =>
      !!document
        .querySelector('[data-browser-terminal]')
        ?.shadowRoot?.querySelector('.xterm-helper-textarea'),
  );
}

// A `value(line)` helper would run inside `page.evaluate` — in the browser,
// not in this Node scope — so it can't be a module-level const here. Each
// value-reading call below inlines `.then((r) => r.value)` instead.

test('flagship pipeline: TS command → structured pipes → typed result', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);
  const count = await page.evaluate(() =>
    window.bt.run("links --limit 20 | filter {|o| $o.text != ''} | length").then((r) => r.value),
  );
  // The demo page is the fixture: 8 anchors, one of them with no text.
  expect(count).toBe(7);

  const rows = (await page.evaluate(() =>
    window.bt.run("links | filter {|o| $o.text != ''} | head 2").then((r) => r.value),
  )) as Array<{ text: string; href: string }>;
  expect(rows).toHaveLength(2);
  expect(rows[0]).toHaveProperty('text');
  expect(rows[0]).toHaveProperty('href');
});

test('grep filters with real regex and fails cleanly on bad patterns', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  // Anchors and alternation are regex, not literals — proving the browser's
  // native RegExp is wired in rather than substring matching.
  expect(
    await page.evaluate(() => window.bt.run("links | grep '^Rust' | length").then((r) => r.value)),
  ).toBe(1);
  expect(
    await page.evaluate(() =>
      window.bt.run("links | grep 'rust|xterm' -i | length").then((r) => r.value),
    ),
  ).toBe(2);
  expect(
    await page.evaluate(() =>
      window.bt.run("links | grep org --on href | length").then((r) => r.value),
    ),
  ).toBe(3);
  expect(
    await page.evaluate(() => window.bt.run("links | grep '^Rust' -v | length").then((r) => r.value)),
  ).toBe(7);

  const err = await page.evaluate(() =>
    window.bt.run("links | grep '('").then(
      () => 'resolved?!',
      (e: Error) => e.message,
    ),
  );
  expect(err).toContain('invalid regex pattern');
  // The engine must survive an invalid pattern.
  expect(await page.evaluate(() => window.bt.run('echo 42').then((r) => r.value))).toBe(42);
});

test('selectors: inline functions, @named, and --on', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  // Inline lambdas project and filter, and compose with everything else.
  expect(
    await page.evaluate(() =>
      window.bt.run("links | filter '(o) => o.text.length > 4' | length").then((r) => r.value),
    ),
  ).toBe(6);
  expect(
    await page.evaluate(() =>
      window.bt.run("links | map '(o) => o.text' | head").then((r) => r.value),
    ),
  ).toBe('Rust language');

  // `--on` narrows what a command looks at while keeping whole rows.
  expect(
    await page.evaluate(() =>
      window.bt.run("links | grep '^Rust' --on text | map href").then((r) => r.value),
    ),
  ).toEqual(['https://www.rust-lang.org/']);

  // A registered function needs no eval — the CSP-safe path.
  expect(
    await page.evaluate(() => window.bt.run("links | map @host | head").then((r) => r.value)),
  ).toBe('www.rust-lang.org');

  // A bad selector name is a clean, suggestive error.
  const err = await page.evaluate(() =>
    window.bt.run('links | map @hostt').then(
      () => 'resolved?!',
      (e: Error) => e.message,
    ),
  );
  expect(err).toContain('did you mean `@host`');
});

test('--help is generated from the signature, and the page shows the real thing', async ({
  page,
}) => {
  await page.goto('/');
  await waitForTerminal(page);

  // Nothing declares a `--help` flag; the evaluator intercepts it before
  // binding, so the text can only have come from the registered signature.
  const help = (await page.evaluate(() =>
    window.bt.run('links --help').then((r) => r.value),
  )) as string;
  expect(help).toContain('Usage:');
  // The command name is colored, so only the arg part is a contiguous run.
  expect(help).toContain('[pattern] [flags]');
  expect(help).toContain('stop after this many links');

  // A command with no flags still gets a usage line rather than an error.
  expect(
    await page.evaluate(() => window.bt.run('fail --help').then((r) => r.value)),
  ).toContain('Usage:');

  // A group name is not a command, but naming it lists what's under it —
  // `mux` used to be an unknown command that suggested `map`.
  const group = (await page.evaluate(() => window.bt.run('mux').then((r) => r.value))) as string;
  expect(group).toContain('`mux` is a command group');
  expect(group).toContain('mux split');
  const typo = await page.evaluate(() =>
    window.bt.run('mux spilt').then(
      () => 'resolved?!',
      (e: Error) => e.message,
    ),
  );
  expect(typo).toContain('`mux` has no subcommand `spilt`');
  expect(typo).toContain('did you mean `mux split`');

  // And the panels on the page are that same output, not a transcription.
  const panels = page.locator('#help-panels details');
  await expect(panels).toHaveCount(2);
  await expect(panels.first()).toContainText('stop after this many links');
  // The ANSI must have been converted, not printed raw.
  await expect(panels.first()).not.toContainText('[1m');
});

test('prefix chord splits the pane', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);
  const paneCount = () =>
    page.evaluate(
      () =>
        document.querySelector('[data-browser-terminal]')!.shadowRoot!.querySelectorAll('.xterm')
          .length,
    );
  expect(await paneCount()).toBe(1);

  await page.evaluate(() => {
    const ta = document
      .querySelector('[data-browser-terminal]')!
      .shadowRoot!.querySelector('.xterm-helper-textarea') as HTMLTextAreaElement;
    ta.focus();
    ta.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'b', keyCode: 66, ctrlKey: true, bubbles: true, cancelable: true }),
    );
    ta.dispatchEvent(
      new KeyboardEvent('keydown', { key: '%', keyCode: 53, shiftKey: true, bubbles: true, cancelable: true }),
    );
  });
  await page.waitForFunction(
    () =>
      document.querySelector('[data-browser-terminal]')!.shadowRoot!.querySelectorAll('.xterm')
        .length === 2,
  );
});

test('session fork shows dock pills and switches back', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);
  await page.evaluate(() => window.bt.run('session new work'));
  await page.waitForFunction(
    () =>
      document.querySelector('[data-browser-terminal]')!.shadowRoot!.querySelectorAll('.pill')
        .length === 2,
  );
  await page.evaluate(() => {
    const root = document.querySelector('[data-browser-terminal]')!.shadowRoot!;
    const pill = [...root.querySelectorAll('.pill')].find((p) => p.textContent === 'main');
    (pill as HTMLElement).click();
  });
  await page.waitForFunction(
    () =>
      document
        .querySelector('[data-browser-terminal]')!
        .shadowRoot!.querySelector('.session-name')!.textContent === '[main]',
  );
});

test('rich TS errors reject run() with message and help', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);
  const message = await page.evaluate(() =>
    window.bt.run('fail').then(
      () => 'resolved?!',
      (e: Error) => e.message,
    ),
  );
  expect(message).toContain('this command always fails');
});

test('dispose removes the panel and rejects further runs', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);
  await page.evaluate(() => window.bt.dispose());
  expect(await page.evaluate(() => !!document.querySelector('[data-browser-terminal]'))).toBe(false);
  const rejected = await page.evaluate(() =>
    window.bt.run('echo hi').then(
      () => false,
      () => true,
    ),
  );
  expect(rejected).toBe(true);
});

test('SECURITY: diagnostics stay out of the pipe and cannot inject escapes', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  await page.evaluate(() => {
    window.bt.registerCommand(
      { name: 'noisy', summary: 'writes to every channel' },
      (_a, _i, ctx) => {
        ctx.log('LOG-LINE');
        ctx.err('ERR-LINE');
        return [{ id: 1 }, { id: 2 }];
      },
    );
  });

  // The pipe carries only the data channel.
  expect(
    await page.evaluate(() => window.bt.run('noisy | length').then((r) => r.value)),
  ).toBe(2);

  const asText = (await page.evaluate(() =>
    window.bt.run('noisy | to json').then((r) => r.value),
  )) as string;
  expect(asText).not.toContain('LOG-LINE');
  expect(asText).not.toContain('ERR-LINE');

  // The caller still receives them, on the channel each was written to.
  const result = await page.evaluate(() => window.bt.run('noisy'));
  expect(result.log).toEqual(['LOG-LINE']);
  expect(result.err).toEqual(['ERR-LINE']);
});

test('SECURITY: a page-controlled diagnostic cannot clear the terminal', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  // Escape sequences in diagnostic text must be stripped before they reach
  // xterm. Prior to the channel work this went through raw, so a command
  // could clear the user's screen.
  const result = await page.evaluate(() => {
    window.bt.registerCommand({ name: 'hostile', summary: 'probe' }, (_a, _i, ctx) => {
      ctx.err('\x1b[2J\x1b[HPAYLOAD\nsecond');
      return null;
    });
    return window.bt.run('hostile');
  });

  // The diagnostic still arrives...
  expect(result.err).toHaveLength(1);
  // ...but stripped of the escape and collapsed to one line.
  expect(result.err[0]).toContain('PAYLOAD');
  expect(result.err[0]).not.toContain('\x1b');
  expect(result.err[0]).not.toContain('[2J');
  expect(result.err[0]).toBe('PAYLOAD second');
});

test('a failed run still hands back the diagnostics it wrote', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  // A failure is exactly when the log leading up to it matters most, so
  // rejecting with only the message would throw away the useful part.
  const result = await page.evaluate(() => {
    window.bt.registerCommand({ name: 'loud-fail', summary: 'probe' }, (_a, _i, ctx) => {
      ctx.log('step one ok');
      ctx.err('about to fail');
      throw { message: 'it broke', help: 'this is the demo failure' };
    });
    return window.bt.run('loud-fail').then(
      () => ({ resolved: true }),
      (e: Error & { log?: string[]; err?: string[] }) => ({
        resolved: false,
        message: e.message,
        isError: e instanceof Error,
        log: e.log,
        err: e.err,
      }),
    );
  });

  expect(result.resolved).toBe(false);
  // Still a real Error, so `catch` and `instanceof` keep working.
  expect(result.isError).toBe(true);
  expect(result.message).toContain('it broke');
  // ...and it carries what the command managed to write first.
  expect(result.log).toEqual(['step one ok']);
  expect(result.err).toEqual(['about to fail']);
});
