/**
 * Smoke suite (milestone 7): proves the wired-together system in a real
 * browser — engine boot, TS command registration, the flagship pipeline,
 * prefix-key splits, session pills, and dispose.
 */
import { expect, test } from '@playwright/test';

declare global {
  interface Window {
    bt: {
      run(line: string): Promise<unknown>;
      dispose(): void;
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

test('flagship pipeline: TS command → structured pipes → typed result', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);
  const count = await page.evaluate(() =>
    window.bt.run("links --limit 20 | filter {|o| $o.text != ''} | length"),
  );
  expect(count).toBe(5);

  const rows = (await page.evaluate(() =>
    window.bt.run("links | filter {|o| $o.text != ''} | head 2"),
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
  expect(await page.evaluate(() => window.bt.run("links | grep '^Rust' | length"))).toBe(1);
  expect(await page.evaluate(() => window.bt.run("links | grep 'rust|xterm' -i | length"))).toBe(2);
  expect(await page.evaluate(() => window.bt.run("links | grep org --on href | length"))).toBe(3);
  expect(await page.evaluate(() => window.bt.run("links | grep '^Rust' -v | length"))).toBe(5);

  const err = await page.evaluate(() =>
    window.bt.run("links | grep '('").then(
      () => 'resolved?!',
      (e: Error) => e.message,
    ),
  );
  expect(err).toContain('invalid regex pattern');
  // The engine must survive an invalid pattern.
  expect(await page.evaluate(() => window.bt.run('echo 42'))).toBe(42);
});

test('selectors: inline functions, @named, and --on', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  // Inline lambdas project and filter, and compose with everything else.
  expect(
    await page.evaluate(() =>
      window.bt.run("links | filter '(o) => o.text.length > 4' | length"),
    ),
  ).toBe(4);
  expect(
    await page.evaluate(() => window.bt.run("links | map '(o) => o.text' | head")),
  ).toBe('Rust language');

  // `--on` narrows what a command looks at while keeping whole rows.
  expect(
    await page.evaluate(() => window.bt.run("links | grep '^Rust' --on text | map href")),
  ).toEqual(['https://www.rust-lang.org/']);

  // A registered function needs no eval — the CSP-safe path.
  expect(
    await page.evaluate(() => window.bt.run("links | map @host | head")),
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
