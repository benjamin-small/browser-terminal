# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: packages/demo/tests/smoke.spec.ts >> selectors: inline functions, @named, and --on
- Location: packages/demo/tests/smoke.spec.ts:101:1

# Error details

```
Error: page.goto: Protocol error (Page.navigate): Cannot navigate to invalid URL
Call log:
  - navigating to "/", waiting until "load"

```

# Test source

```ts
  2   |  * Smoke suite (milestone 7): proves the wired-together system in a real
  3   |  * browser — engine boot, TS command registration, the flagship pipeline,
  4   |  * prefix-key splits, session pills, and dispose.
  5   |  */
  6   | import { expect, test } from '@playwright/test';
  7   | 
  8   | declare global {
  9   |   interface Window {
  10  |     bt: {
  11  |       run(line: string): Promise<{ value: unknown; log: string[]; err: string[] }>;
  12  |       dispose(): void;
  13  |       registerCommand(
  14  |         spec: { name: string; summary?: string },
  15  |         fn: (
  16  |           args: unknown,
  17  |           input: unknown,
  18  |           ctx: { log(s: string): void; err(s: string): void; emit(s: string): void },
  19  |         ) => unknown,
  20  |       ): void;
  21  |     };
  22  |   }
  23  | }
  24  | 
  25  | function shadow(selector: string) {
  26  |   return `document.querySelector('[data-browser-terminal]').shadowRoot.querySelector('${selector}')`;
  27  | }
  28  | 
  29  | async function waitForTerminal(page: import('@playwright/test').Page) {
  30  |   await page.waitForFunction(
  31  |     () =>
  32  |       !!document
  33  |         .querySelector('[data-browser-terminal]')
  34  |         ?.shadowRoot?.querySelector('.xterm-helper-textarea'),
  35  |   );
  36  | }
  37  | 
  38  | // A `value(line)` helper would run inside `page.evaluate` — in the browser,
  39  | // not in this Node scope — so it can't be a module-level const here. Each
  40  | // value-reading call below inlines `.then((r) => r.value)` instead.
  41  | 
  42  | test('flagship pipeline: TS command → structured pipes → typed result', async ({ page }) => {
  43  |   await page.goto('/');
  44  |   await waitForTerminal(page);
  45  |   const count = await page.evaluate(() =>
  46  |     window.bt.run("links --limit 20 | filter {|o| $o.text != ''} | length").then((r) => r.value),
  47  |   );
  48  |   // The demo page is the fixture: 8 anchors, one of them with no text.
  49  |   expect(count).toBe(7);
  50  | 
  51  |   const rows = (await page.evaluate(() =>
  52  |     window.bt.run("links | filter {|o| $o.text != ''} | head 2").then((r) => r.value),
  53  |   )) as Array<{ text: string; href: string }>;
  54  |   expect(rows).toHaveLength(2);
  55  |   expect(rows[0]).toHaveProperty('text');
  56  |   expect(rows[0]).toHaveProperty('href');
  57  | });
  58  | 
  59  | test('grep filters with real regex and fails cleanly on bad patterns', async ({ page }) => {
  60  |   await page.goto('/');
  61  |   await waitForTerminal(page);
  62  | 
  63  |   // Anchors and alternation are regex, not literals — proving the browser's
  64  |   // native RegExp is wired in rather than substring matching.
  65  |   //
  66  |   // Batch-model note (stage 3, streaming): a stream of exactly one item is
  67  |   // indistinguishable from a bare scalar at a collecting boundary (see
  68  |   // `stream::collect` / docs/superpowers/specs/2026-07-24-streaming-commands-design.md),
  69  |   // so counting a single-row grep match via `| length` now hits a type
  70  |   // error instead of yielding 1 — that's the documented, approved edge
  71  |   // case ("`echo 5 | length`-class edges may change"), not a regression.
  72  |   // Assert the anchored match directly instead of counting it.
  73  |   expect(
  74  |     await page.evaluate(() => window.bt.run("links | grep '^Rust'").then((r) => r.value)),
  75  |   ).toMatchObject({ text: 'Rust language' });
  76  |   expect(
  77  |     await page.evaluate(() =>
  78  |       window.bt.run("links | grep 'rust|xterm' -i | length").then((r) => r.value),
  79  |     ),
  80  |   ).toBe(2);
  81  |   expect(
  82  |     await page.evaluate(() =>
  83  |       window.bt.run("links | grep org --on href | length").then((r) => r.value),
  84  |     ),
  85  |   ).toBe(3);
  86  |   expect(
  87  |     await page.evaluate(() => window.bt.run("links | grep '^Rust' -v | length").then((r) => r.value)),
  88  |   ).toBe(7);
  89  | 
  90  |   const err = await page.evaluate(() =>
  91  |     window.bt.run("links | grep '('").then(
  92  |       () => 'resolved?!',
  93  |       (e: Error) => e.message,
  94  |     ),
  95  |   );
  96  |   expect(err).toContain('invalid regex pattern');
  97  |   // The engine must survive an invalid pattern.
  98  |   expect(await page.evaluate(() => window.bt.run('echo 42').then((r) => r.value))).toBe(42);
  99  | });
  100 | 
  101 | test('selectors: inline functions, @named, and --on', async ({ page }) => {
> 102 |   await page.goto('/');
      |              ^ Error: page.goto: Protocol error (Page.navigate): Cannot navigate to invalid URL
  103 |   await waitForTerminal(page);
  104 | 
  105 |   // Inline lambdas project and filter, and compose with everything else.
  106 |   expect(
  107 |     await page.evaluate(() =>
  108 |       window.bt.run("links | filter '(o) => o.text.length > 4' | length").then((r) => r.value),
  109 |     ),
  110 |   ).toBe(6);
  111 |   expect(
  112 |     await page.evaluate(() =>
  113 |       window.bt.run("links | map '(o) => o.text' | head").then((r) => r.value),
  114 |     ),
  115 |   ).toBe('Rust language');
  116 | 
  117 |   // `--on` narrows what a command looks at while keeping whole rows.
  118 |   //
  119 |   // Batch-model note (stage 3, streaming): `grep` here matches exactly one
  120 |   // row, so the stream reaching the terminal collector has exactly one
  121 |   // item — same rule that unwraps a no-arg `head` to a scalar rather than
  122 |   // a 1-list (see docs/superpowers/specs/2026-07-24-streaming-commands-design.md).
  123 |   // Was `.toEqual([...])` pre-streaming; now the bare string is correct.
  124 |   expect(
  125 |     await page.evaluate(() =>
  126 |       window.bt.run("links | grep '^Rust' --on text | map href").then((r) => r.value),
  127 |     ),
  128 |   ).toBe('https://www.rust-lang.org/');
  129 | 
  130 |   // A registered function needs no eval — the CSP-safe path.
  131 |   expect(
  132 |     await page.evaluate(() => window.bt.run("links | map @host | head").then((r) => r.value)),
  133 |   ).toBe('www.rust-lang.org');
  134 | 
  135 |   // A bad selector name is a clean, suggestive error.
  136 |   const err = await page.evaluate(() =>
  137 |     window.bt.run('links | map @hostt').then(
  138 |       () => 'resolved?!',
  139 |       (e: Error) => e.message,
  140 |     ),
  141 |   );
  142 |   expect(err).toContain('did you mean `@host`');
  143 | });
  144 | 
  145 | test('--help is generated from the signature, and the page shows the real thing', async ({
  146 |   page,
  147 | }) => {
  148 |   await page.goto('/');
  149 |   await waitForTerminal(page);
  150 | 
  151 |   // Nothing declares a `--help` flag; the evaluator intercepts it before
  152 |   // binding, so the text can only have come from the registered signature.
  153 |   const help = (await page.evaluate(() =>
  154 |     window.bt.run('links --help').then((r) => r.value),
  155 |   )) as string;
  156 |   expect(help).toContain('Usage:');
  157 |   // The command name is colored, so only the arg part is a contiguous run.
  158 |   expect(help).toContain('[pattern] [flags]');
  159 |   expect(help).toContain('stop after this many links');
  160 | 
  161 |   // A command with no flags still gets a usage line rather than an error.
  162 |   expect(
  163 |     await page.evaluate(() => window.bt.run('fail --help').then((r) => r.value)),
  164 |   ).toContain('Usage:');
  165 | 
  166 |   // A group name is not a command, but naming it lists what's under it —
  167 |   // `mux` used to be an unknown command that suggested `map`.
  168 |   const group = (await page.evaluate(() => window.bt.run('mux').then((r) => r.value))) as string;
  169 |   expect(group).toContain('`mux` is a command group');
  170 |   expect(group).toContain('mux split');
  171 |   const typo = await page.evaluate(() =>
  172 |     window.bt.run('mux spilt').then(
  173 |       () => 'resolved?!',
  174 |       (e: Error) => e.message,
  175 |     ),
  176 |   );
  177 |   expect(typo).toContain('`mux` has no subcommand `spilt`');
  178 |   expect(typo).toContain('did you mean `mux split`');
  179 | 
  180 |   // And the panels on the page are that same output, not a transcription.
  181 |   const panels = page.locator('#help-panels details');
  182 |   await expect(panels).toHaveCount(2);
  183 |   await expect(panels.first()).toContainText('stop after this many links');
  184 |   // The ANSI must have been converted, not printed raw.
  185 |   await expect(panels.first()).not.toContainText('[1m');
  186 | });
  187 | 
  188 | test('prefix chord splits the pane', async ({ page }) => {
  189 |   await page.goto('/');
  190 |   await waitForTerminal(page);
  191 |   const paneCount = () =>
  192 |     page.evaluate(
  193 |       () =>
  194 |         document.querySelector('[data-browser-terminal]')!.shadowRoot!.querySelectorAll('.xterm')
  195 |           .length,
  196 |     );
  197 |   expect(await paneCount()).toBe(1);
  198 | 
  199 |   await page.evaluate(() => {
  200 |     const ta = document
  201 |       .querySelector('[data-browser-terminal]')!
  202 |       .shadowRoot!.querySelector('.xterm-helper-textarea') as HTMLTextAreaElement;
```