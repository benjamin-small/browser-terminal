import { test as base, expect } from '@playwright/test';

// WebKit on macOS exposes getDirectory in ephemeral contexts but rejects it.
// Use a disposable persistent profile to exercise actual OPFS in that engine.
const test = base.extend({
  context: async ({ browser, browserName, playwright, baseURL }, use, testInfo) => {
    const profile = browserName === 'webkit' ? testInfo.outputPath('browser-profile') : undefined;
    const context = profile
      ? await playwright.webkit.launchPersistentContext(profile, { headless: true, baseURL })
      : await browser.newContext({ baseURL });
    try { await use(context); }
    finally { await context.close(); }
  },
});

test.beforeAll(async ({ browser }) => { console.log('Filesystem test browser:', browser.version()); });

test.beforeEach(async ({ page }) => {
  await page.goto('/');
  await page.waitForFunction(() => !!window.bt && !!window.filesystem);
  await page.getByRole('button', { name: 'Open scratch', exact: true }).click();
  await expect(page.getByText('Scratch storage mounted at /scratch. Try edit welcome.txt.', { exact: true })).toBeVisible();
});

test('real OPFS navigation, redirects and sessions through WASM', async ({ page }) => {
  await page.getByRole('button', { name: 'Enable file redirects', exact: true }).click();
  const result = await page.evaluate(async () => {
    const bt = window.bt;
    const first = (await bt.run('pwd')).value;
    await bt.run('echo hello > greeting.txt; echo world >> greeting.txt');
    const text = (await bt.run('cat greeting.txt')).value;
    const list = (await bt.run("ls | filter {|e| $e.name == 'greeting.txt'} | length")).value;
    await bt.run('session new files');
    const second = (await bt.run('pwd')).value;
    const bytes = (await bt.run('read-bytes /dev/zero --length 4')).value as Uint8Array;
    return { first, text, list, second, bytes: [...bytes] };
  });
  expect(result).toEqual({ first: '/scratch', text: 'helloworld', list: 1, second: '/', bytes: [0, 0, 0, 0] });
});

test('editor saves CRLF and BOM to OPFS and detects an external edit', async ({ page }) => {
  await page.evaluate(async () => {
    const session = window.bt.snapshot!.sessions.find(s => s.active)!.id;
    await window.filesystem.write('/scratch/edit.txt', new Uint8Array([239, 187, 191, 65, 13, 10]), { session });
    await window.bt.run('edit edit.txt');
  });
  const editor = page.getByRole('dialog', { name: 'Edit /scratch/edit.txt', exact: true });
  const input = editor.getByRole('textbox');
  await expect(input).toHaveValue('A\n');
  await page.screenshot({ path: test.info().outputPath('filesystem-editor.png') });
  await input.fill('B\n');
  await input.press('Control+s');
  await expect(editor.getByRole('status')).toHaveText('Saved');
  const saved = await page.evaluate(async () => {
    const session = window.bt.snapshot!.sessions.find(s => s.active)!.id;
    return [...await window.filesystem.readBytes('/scratch/edit.txt', { session })];
  });
  expect(saved).toEqual([239, 187, 191, 66, 13, 10]);
  await page.evaluate(async () => {
    const session = window.bt.snapshot!.sessions.find(s => s.active)!.id;
    await window.filesystem.write('/scratch/edit.txt', new TextEncoder().encode('external'), { session });
  });
  await input.fill('my unsaved work'); await editor.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(editor.getByRole('status')).toContainText('changed externally');
  await expect(input).toHaveValue('my unsaved work');
  page.once('dialog', dialog => dialog.dismiss());
  await editor.getByRole('button', { name: 'Close', exact: true }).click(); await expect(editor).toBeVisible();
  page.once('dialog', dialog => dialog.accept());
  await editor.getByRole('button', { name: 'Reload', exact: true }).click(); await expect(input).toHaveValue('external');
});

test('unmount retains dirty editor text and rejects writes', async ({ page }) => {
  await page.evaluate(() => window.bt.run('edit welcome.txt'));
  const editor = page.getByRole('dialog', { name: 'Edit /scratch/welcome.txt', exact: true });
  await editor.getByRole('textbox').fill('recover me');
  await page.evaluate(() => window.filesystem.unmount('scratch'));
  await expect(editor.getByRole('textbox')).toHaveValue('recover me');
  await expect(editor.getByRole('button', { name: 'Save', exact: true })).toBeDisabled();
  await expect(editor.getByRole('button', { name: 'Export text', exact: true })).toBeEnabled();
  expect(await page.evaluate(async () => (await window.bt.run('pwd')).value)).toBe('/');
});

test('command ownership protects host replacements and builtin collisions', async ({ page }) => {
  const result = await page.evaluate(async () => {
    let duplicate = false; let builtin = false;
    try { window.bt.registerOwnedCommand({ name: 'ls' }, () => 'bad'); } catch { duplicate = true; }
    try { window.bt.registerOwnedCommand({ name: 'echo' }, () => 'bad'); } catch { builtin = true; }
    window.bt.registerCommand({ name: 'ls' }, () => 'replacement');
    window.filesystem.dispose();
    return { duplicate, builtin, result: (await window.bt.run('ls')).value };
  });
  expect(result).toEqual({ duplicate: true, builtin: true, result: 'replacement' });
});

test('failed pipelines never alter an existing OPFS file', async ({ page }) => {
  await page.getByRole('button', { name: 'Enable file redirects', exact: true }).click();
  expect(await page.evaluate(async () => {
    await window.bt.run('echo original > preserved');
    window.bt.registerCommand({ name: 'fs-fail' }, () => { throw new Error('failure'); });
    try { await window.bt.run('fs-fail > preserved'); } catch { /* Expected. */ }
    return (await window.bt.run('cat preserved')).value;
  })).toBe('original');
});


test('unsupported local picker leaves scratch and the terminal usable', async ({ page }) => {
  await page.addInitScript(() => { Object.defineProperty(window, 'showDirectoryPicker', { value: undefined, configurable: true }); });
  await page.reload();
  await expect(page.getByRole('button', { name: 'Connect folder', exact: true })).toBeDisabled();
  await expect(page.getByText('Local folder access is unavailable here. Try private scratch storage.', { exact: true })).toBeVisible();
  expect(await page.evaluate(async () => (await window.bt.run('echo still-works')).value)).toBe('still-works');
});
