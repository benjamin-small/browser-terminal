# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: packages/demo/tests/smoke.spec.ts >> prefix chord splits the pane
- Location: packages/demo/tests/smoke.spec.ts:188:1

# Error details

```
Error: page.goto: Protocol error (Page.navigate): Cannot navigate to invalid URL
Call log:
  - navigating to "/", waiting until "load"

```

# Test source

```ts
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
  102 |   await page.goto('/');
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
> 189 |   await page.goto('/');
      |              ^ Error: page.goto: Protocol error (Page.navigate): Cannot navigate to invalid URL
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
  203 |     ta.focus();
  204 |     ta.dispatchEvent(
  205 |       new KeyboardEvent('keydown', { key: 'b', keyCode: 66, ctrlKey: true, bubbles: true, cancelable: true }),
  206 |     );
  207 |     ta.dispatchEvent(
  208 |       new KeyboardEvent('keydown', { key: '%', keyCode: 53, shiftKey: true, bubbles: true, cancelable: true }),
  209 |     );
  210 |   });
  211 |   await page.waitForFunction(
  212 |     () =>
  213 |       document.querySelector('[data-browser-terminal]')!.shadowRoot!.querySelectorAll('.xterm')
  214 |         .length === 2,
  215 |   );
  216 | });
  217 | 
  218 | test('session fork shows dock pills and switches back', async ({ page }) => {
  219 |   await page.goto('/');
  220 |   await waitForTerminal(page);
  221 |   await page.evaluate(() => window.bt.run('session new work'));
  222 |   await page.waitForFunction(
  223 |     () =>
  224 |       document.querySelector('[data-browser-terminal]')!.shadowRoot!.querySelectorAll('.pill')
  225 |         .length === 2,
  226 |   );
  227 |   await page.evaluate(() => {
  228 |     const root = document.querySelector('[data-browser-terminal]')!.shadowRoot!;
  229 |     const pill = [...root.querySelectorAll('.pill')].find((p) => p.textContent === 'main');
  230 |     (pill as HTMLElement).click();
  231 |   });
  232 |   await page.waitForFunction(
  233 |     () =>
  234 |       document
  235 |         .querySelector('[data-browser-terminal]')!
  236 |         .shadowRoot!.querySelector('.session-name')!.textContent === '[main]',
  237 |   );
  238 | });
  239 | 
  240 | test('rich TS errors reject run() with message and help', async ({ page }) => {
  241 |   await page.goto('/');
  242 |   await waitForTerminal(page);
  243 |   const message = await page.evaluate(() =>
  244 |     window.bt.run('fail').then(
  245 |       () => 'resolved?!',
  246 |       (e: Error) => e.message,
  247 |     ),
  248 |   );
  249 |   expect(message).toContain('this command always fails');
  250 | });
  251 | 
  252 | test('dispose removes the panel and rejects further runs', async ({ page }) => {
  253 |   await page.goto('/');
  254 |   await waitForTerminal(page);
  255 |   await page.evaluate(() => window.bt.dispose());
  256 |   expect(await page.evaluate(() => !!document.querySelector('[data-browser-terminal]'))).toBe(false);
  257 |   const rejected = await page.evaluate(() =>
  258 |     window.bt.run('echo hi').then(
  259 |       () => false,
  260 |       () => true,
  261 |     ),
  262 |   );
  263 |   expect(rejected).toBe(true);
  264 | });
  265 | 
  266 | test('SECURITY: diagnostics stay out of the pipe and cannot inject escapes', async ({ page }) => {
  267 |   await page.goto('/');
  268 |   await waitForTerminal(page);
  269 | 
  270 |   await page.evaluate(() => {
  271 |     window.bt.registerCommand(
  272 |       { name: 'noisy', summary: 'writes to every channel' },
  273 |       (_a, _i, ctx) => {
  274 |         ctx.log('LOG-LINE');
  275 |         ctx.err('ERR-LINE');
  276 |         return [{ id: 1 }, { id: 2 }];
  277 |       },
  278 |     );
  279 |   });
  280 | 
  281 |   // The pipe carries only the data channel.
  282 |   expect(
  283 |     await page.evaluate(() => window.bt.run('noisy | length').then((r) => r.value)),
  284 |   ).toBe(2);
  285 | 
  286 |   const asText = (await page.evaluate(() =>
  287 |     window.bt.run('noisy | to json').then((r) => r.value),
  288 |   )) as string;
  289 |   expect(asText).not.toContain('LOG-LINE');
```