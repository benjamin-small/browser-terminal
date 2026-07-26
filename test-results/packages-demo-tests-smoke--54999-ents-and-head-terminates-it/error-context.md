# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: packages/demo/tests/smoke.spec.ts >> watch streams DOM events and head terminates it
- Location: packages/demo/tests/smoke.spec.ts:441:1

# Error details

```
Error: page.goto: Protocol error (Page.navigate): Cannot navigate to invalid URL
Call log:
  - navigating to "/", waiting until "load"

```

# Test source

```ts
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
  356 |   await page.goto('/');
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
> 442 |   await page.goto('/');
      |              ^ Error: page.goto: Protocol error (Page.navigate): Cannot navigate to invalid URL
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
  457 | 
  458 |   // The listener must be gone and the source must still work cleanly.
  459 |   const again = page.evaluate(() =>
  460 |     window.bt.run('watch click | head 1').then((r) => r.value),
  461 |   );
  462 |   await page.waitForTimeout(150);
  463 |   await page.mouse.click(10, 10);
  464 |   expect(await again).toEqual({ type: 'click', target: expect.any(String) });
  465 | });
  466 | 
```