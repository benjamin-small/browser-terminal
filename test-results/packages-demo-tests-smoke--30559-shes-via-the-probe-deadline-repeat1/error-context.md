# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: packages/demo/tests/smoke.spec.ts >> a slow source paints before it finishes, via the probe deadline
- Location: packages/demo/tests/smoke.spec.ts:355:1

# Error details

```
Error: page.goto: Protocol error (Page.navigate): Cannot navigate to invalid URL
Call log:
  - navigating to "/", waiting until "load"

```

# Test source

```ts
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
  290 |   expect(asText).not.toContain('ERR-LINE');
  291 | 
  292 |   // The caller still receives them, on the channel each was written to.
  293 |   const result = await page.evaluate(() => window.bt.run('noisy'));
  294 |   expect(result.log).toEqual(['LOG-LINE']);
  295 |   expect(result.err).toEqual(['ERR-LINE']);
  296 | });
  297 | 
  298 | test('SECURITY: a page-controlled diagnostic cannot clear the terminal', async ({ page }) => {
  299 |   await page.goto('/');
  300 |   await waitForTerminal(page);
  301 | 
  302 |   // Escape sequences in diagnostic text must be stripped before they reach
  303 |   // xterm. Prior to the channel work this went through raw, so a command
  304 |   // could clear the user's screen.
  305 |   const result = await page.evaluate(() => {
  306 |     window.bt.registerCommand({ name: 'hostile', summary: 'probe' }, (_a, _i, ctx) => {
  307 |       ctx.err('\x1b[2J\x1b[HPAYLOAD\nsecond');
  308 |       return null;
  309 |     });
  310 |     return window.bt.run('hostile');
  311 |   });
  312 | 
  313 |   // The diagnostic still arrives...
  314 |   expect(result.err).toHaveLength(1);
  315 |   // ...but stripped of the escape and collapsed to one line.
  316 |   expect(result.err[0]).toContain('PAYLOAD');
  317 |   expect(result.err[0]).not.toContain('\x1b');
  318 |   expect(result.err[0]).not.toContain('[2J');
  319 |   expect(result.err[0]).toBe('PAYLOAD second');
  320 | });
  321 | 
  322 | test('a failed run still hands back the diagnostics it wrote', async ({ page }) => {
  323 |   await page.goto('/');
  324 |   await waitForTerminal(page);
  325 | 
  326 |   // A failure is exactly when the log leading up to it matters most, so
  327 |   // rejecting with only the message would throw away the useful part.
  328 |   const result = await page.evaluate(() => {
  329 |     window.bt.registerCommand({ name: 'loud-fail', summary: 'probe' }, (_a, _i, ctx) => {
  330 |       ctx.log('step one ok');
  331 |       ctx.err('about to fail');
  332 |       throw { message: 'it broke', help: 'this is the demo failure' };
  333 |     });
  334 |     return window.bt.run('loud-fail').then(
  335 |       () => ({ resolved: true }),
  336 |       (e: Error & { log?: string[]; err?: string[] }) => ({
  337 |         resolved: false,
  338 |         message: e.message,
  339 |         isError: e instanceof Error,
  340 |         log: e.log,
  341 |         err: e.err,
  342 |       }),
  343 |     );
  344 |   });
  345 | 
  346 |   expect(result.resolved).toBe(false);
  347 |   // Still a real Error, so `catch` and `instanceof` keep working.
  348 |   expect(result.isError).toBe(true);
  349 |   expect(result.message).toContain('it broke');
  350 |   // ...and it carries what the command managed to write first.
  351 |   expect(result.log).toEqual(['step one ok']);
  352 |   expect(result.err).toEqual(['about to fail']);
  353 | });
  354 | 
  355 | test('a slow source paints before it finishes, via the probe deadline', async ({ page }) => {
> 356 |   await page.goto('/');
      |              ^ Error: page.goto: Protocol error (Page.navigate): Cannot navigate to invalid URL
  357 |   await waitForTerminal(page);
  358 | 
  359 |   await page.evaluate(() => {
  360 |     // 3 rows over ~1.2s: far fewer than PROBE_ROWS, so only the host's
  361 |     // 150ms deadline can make anything paint before the stream ends.
  362 |     window.bt.registerCommand({ name: 'drip', summary: 'slow rows' }, async function* () {
  363 |       for (let i = 0; i < 3; i++) {
  364 |         await new Promise((r) => setTimeout(r, 400));
  365 |         yield { id: i };
  366 |       }
  367 |     });
  368 |   });
  369 | 
  370 |   // Type it into the pane so it renders there (run() is programmatic and
  371 |   // does not paint). Manually constructing InputEvents on the hidden
  372 |   // textarea does not reach xterm's own input handling (verified: it
  373 |   // needs real key events, not a synthesized `input` event with a value
  374 |   // set out from under it) -- Playwright's real key-event dispatch does,
  375 |   // because it pierces the open shadow root and drives the textarea the
  376 |   // same way a real keystroke would.
  377 |   const ta = page.locator('[data-browser-terminal]').locator('.xterm-helper-textarea');
  378 |   await ta.click();
  379 |   await ta.pressSequentially('drip');
  380 |   await ta.press('Enter');
  381 | 
  382 |   // The header must appear WELL BEFORE the stream could have finished
  383 |   // (3 x 400ms = 1200ms). Waiting up to 900ms proves it did not wait for
  384 |   // the end.
  385 |   await page.waitForFunction(
  386 |     () => {
  387 |       const term = document.querySelector('[data-browser-terminal]')!.shadowRoot!;
  388 |       const text = term.querySelector('.xterm-rows')?.textContent ?? '';
  389 |       return text.includes('id');
  390 |     },
  391 |     null,
  392 |     { timeout: 900 },
  393 |   );
  394 | });
  395 | 
  396 | test('a fast source stays responsive', async ({ page }) => {
  397 |   await page.goto('/');
  398 |   await waitForTerminal(page);
  399 | 
  400 |   await page.evaluate(() => {
  401 |     window.bt.registerCommand({ name: 'flood', summary: '2000 rows fast' }, async function* () {
  402 |       for (let i = 0; i < 2000; i++) {
  403 |         yield { id: i };
  404 |       }
  405 |     });
  406 |   });
  407 | 
  408 |   // Type it into the pane rather than `run('flood | length')`: `run()`
  409 |   // collects through CollectingConsumer/CollectingSink, which never touches
  410 |   // PaneSink::ready() -- the cooperative-yield throttle this test means to
  411 |   // exercise. `| length` would also collapse the 2000 rows to one scalar
  412 |   // before they ever reach the pane, so this runs `flood` bare and lets all
  413 |   // 2000 records flow through the progressive path, one yield apiece.
  414 |   const ta = page.locator('[data-browser-terminal]').locator('.xterm-helper-textarea');
  415 |   await ta.click();
  416 |   const started = Date.now();
  417 |   await ta.pressSequentially('flood');
  418 |   await ta.press('Enter');
  419 | 
  420 |   // The point is that painting 2000 rows, one cooperative yield each,
  421 |   // completes rather than hanging the tab. Generous bound.
  422 |   //
  423 |   // Can't look for the literal id "1999": the pane in this viewport is
  424 |   // narrow enough that numeric cells truncate (e.g. to "1…"), so the last
  425 |   // row's value never appears verbatim. The bottom border only appears once
  426 |   // the table closes, and a fresh prompt only reprints after `finish_pane`
  427 |   // -- together they're a completion signal that doesn't depend on the
  428 |   // pane's column width.
  429 |   await page.waitForFunction(
  430 |     () => {
  431 |       const term = document.querySelector('[data-browser-terminal]')!.shadowRoot!;
  432 |       const text = term.querySelector('.xterm-rows')?.textContent ?? '';
  433 |       return text.includes('└') && text.trimEnd().endsWith('❯');
  434 |     },
  435 |     null,
  436 |     { timeout: 15000 },
  437 |   );
  438 |   expect(Date.now() - started).toBeLessThan(15000);
  439 | });
  440 | 
  441 | test('watch streams DOM events and head terminates it', async ({ page }) => {
  442 |   await page.goto('/');
  443 |   await waitForTerminal(page);
  444 | 
  445 |   const done = page.evaluate(() =>
  446 |     window.bt.run('watch click | map type | head 3').then((r) => r.value),
  447 |   );
  448 | 
  449 |   await page.waitForTimeout(200);
  450 |   for (let i = 0; i < 4; i++) {
  451 |     await page.mouse.click(10, 10);
  452 |     await page.waitForTimeout(40);
  453 |   }
  454 | 
  455 |   const value = await done;
  456 |   expect(value).toEqual(['click', 'click', 'click']);
```