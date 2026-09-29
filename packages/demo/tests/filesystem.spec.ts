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
  await expect(page.getByRole('region', { name: 'Browser filesystem' }).getByRole('status')).toContainText('Browser files ready at /scratch.');
});

test('real OPFS navigation, redirects and sessions through WASM', async ({ page }) => {
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
  expect(result).toEqual({ first: '/scratch', text: 'helloworld', list: 1, second: '/scratch', bytes: [0, 0, 0, 0] });
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
  await expect(page.getByRole('button', { name: 'Connect local folder', exact: true })).toBeDisabled();
  await expect(page.getByRole('region', { name: 'Browser filesystem' }).getByRole('status')).toContainText('Connecting local folders is unavailable');
  expect(await page.evaluate(async () => (await window.bt.run('echo still-works')).value)).toBe('still-works');
});


test('startup needs no picker or permission requests and browser files survive reload', async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(window, 'showDirectoryPicker', { value: () => { throw new Error('Unexpected folder picker'); }, configurable: true });
    Object.defineProperty(FileSystemHandle.prototype, 'requestPermission', { value: () => { throw new Error('Unexpected permission request'); }, configurable: true });
  });
  await page.reload();
  await page.waitForFunction(() => !!window.bt);
  expect(await page.evaluate(async () => (await window.bt.run('pwd')).value)).toBe('/scratch');
  await page.evaluate(() => window.bt.run('echo remembered > persisted.txt; edit persisted.txt'));
  const editor = page.getByRole('dialog', { name: 'Edit /scratch/persisted.txt', exact: true });
  await expect(editor.getByRole('button', { name: 'Enable writes', exact: true })).toBeHidden();
  await editor.getByRole('textbox').fill('saved without prompts');
  await editor.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(editor.getByRole('status')).toHaveText('Saved');
  await page.reload(); await page.waitForFunction(() => !!window.bt);
  expect(await page.evaluate(async () => (await window.bt.run('cat persisted.txt')).value)).toBe('saved without prompts');
});

test('unavailable browser storage leaves the terminal usable without opening a picker', async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(StorageManager.prototype, 'getDirectory', { value: () => Promise.reject(new Error('Storage unavailable')), configurable: true });
    Object.defineProperty(window, 'showDirectoryPicker', { value: () => { throw new Error('Unexpected picker'); }, configurable: true });
  });
  await page.reload(); await page.waitForFunction(() => !!window.bt);
  await expect(page.getByRole('region', { name: 'Browser filesystem' }).getByRole('status')).toContainText('Could not open browser storage');
  expect(await page.evaluate(async () => (await window.bt.run('pwd')).value)).toBe('/');
  expect(await page.evaluate(async () => (await window.bt.run('echo still-works')).value)).toBe('still-works');
});

test('Tab completes cat and registered commands, including pipelines', async ({ page }) => {
  const root = page.locator('[data-browser-terminal]');
  const input = root.locator('[data-active="true"] .xterm-helper-textarea');
  const rows = root.locator('[data-active="true"] .xterm-rows');
  await page.evaluate(async () => {
    await window.bt.run('echo completion-content > completion.txt');
    window.bt.registerCommand({ name: 'sample-completion' }, () => 'custom-completion-result');
  });
  await input.pressSequentially('ca');
  await input.press('Tab');
  await expect(rows).toContainText('/scratch ❯ cat ');
  await input.pressSequentially('completion.txt');
  await input.press('Enter');
  await expect(rows).toContainText('completion-content');
  await input.pressSequentially('sample-comp');
  await input.press('Tab');
  await input.press('Enter');
  await expect(rows).toContainText('custom-completion-result');
  await input.pressSequentially('echo mixed | str up');
  await input.press('Tab');
  await input.press('Enter');
  await expect(rows).toContainText('MIXED');
});

test('directory prompt follows cd, split panes, sessions and unmount', async ({ page }) => {
  const root = page.locator('[data-browser-terminal]');
  const rows = () => root.locator('[data-active="true"] .xterm-rows:visible');
  const input = () => root.locator('[data-active="true"] .xterm-helper-textarea:visible');
  await expect(rows()).toHaveText(/\/scratch ❯\s*$/);
  await input().pressSequentially('cd /');
  await input().press('Enter');
  await expect(rows()).toHaveText(/\/ ❯\s*$/);
  await page.evaluate(() => window.bt.run('mux split --right'));
  await expect(rows()).toHaveText(/\/ ❯\s*$/);
  await page.evaluate(() => window.bt.run('session new elsewhere'));
  await expect(rows()).toHaveText(/\/scratch ❯\s*$/);
  await root.getByText('main', { exact: true }).click();
  await expect(rows()).toHaveText(/\/ ❯\s*$/);
  await input().pressSequentially('cd /scratch');
  await input().press('Enter');
  for (const pane of await root.locator('.xterm-rows:visible').all()) await expect(pane).toHaveText(/\/scratch ❯\s*$/);
  await page.evaluate(() => window.filesystem.unmount('scratch'));
  for (const pane of await root.locator('.xterm-rows:visible').all()) {
    await expect(pane).toHaveText(/\/ ❯\s*$/);
  }
});

test('Tab completes file arguments, quoted names, directories and flags', async ({ page }) => {
  const root = page.locator('[data-browser-terminal]');
  const input = root.locator('[data-active="true"] .xterm-helper-textarea:visible');
  const rows = root.locator('[data-active="true"] .xterm-rows:visible');
  await page.evaluate(async () => {
    const session = window.bt.snapshot!.sessions.find(s => s.active)!.id;
    const root = await navigator.storage.getDirectory();
    const dir = await root.getDirectoryHandle('browser-terminal-demo');
    await dir.getDirectoryHandle('folder space', { create: true });
    await window.filesystem.write('/scratch/notes $unsafe;\'quote".txt', new TextEncoder().encode('quoted-file-content'), { session });
  });
  await input.pressSequentially('cat ');
  await input.press('Tab');
  await expect(rows).toContainText('welcome.txt');
  await expect(rows).toHaveText(/cat\s*$/);
  await input.pressSequentially('wel');
  await input.press('Tab');
  await expect(rows).toHaveText(/cat welcome.txt\s*$/);
  await input.press('Control+u');
  await input.pressSequentially('cat no');
  await input.press('Tab');
  await expect(rows).toHaveText(/cat "notes .*\.txt"\s*$/);
  await input.press('Enter');
  await expect(rows).toContainText('quoted-file-content');
  await input.pressSequentially('ls --lo');
  await input.press('Tab');
  await expect(rows).toHaveText(/ls --long\s*$/);
  await input.press('Control+u');
  await input.pressSequentially('cd fol');
  await input.press('Tab');
  await expect(rows).toHaveText(/cd "folder space\/"\s*$/);
  await input.press('Enter');
  await expect(rows).toHaveText(/\/scratch\/folder space ❯\s*$/);
});

test('command argument providers support values and discard results after typing', async ({ page }) => {
  const root = page.locator('[data-browser-terminal]');
  const input = root.locator('[data-active="true"] .xterm-helper-textarea:visible');
  const rows = root.locator('[data-active="true"] .xterm-rows:visible');
  await page.evaluate(() => {
    window.bt.registerCommand({ name: 'choose', required: [{ name: 'mode', shape: 'str' }], flags: [{ long: 'enabled', shape: 'bool' }] }, args => args.positionals[0]);
    window.bt.addCompletionProvider(async context => {
      if (context.command !== 'choose' || context.flag) return [];
      if (context.prefix === 'slow') {
        await new Promise(resolve => setTimeout(resolve, 150));
        document.body.dataset.completionResolved = 'true';
        return [{ value: 'slow-result' }];
      }
      return [{ value: 'dark' }, { value: 'light' }];
    });
  });
  await input.pressSequentially('choose da');
  await input.press('Tab');
  await expect(rows).toHaveText(/choose dark\s*$/);
  await input.press('Control+u');
  await input.pressSequentially('choose --enabled tr');
  await input.press('Tab');
  await expect(rows).toHaveText(/choose --enabled true\s*$/);
  await input.press('Control+u');
  await input.pressSequentially('choose slow');
  await input.press('Tab');
  await input.pressSequentially('new-input');
  await expect(page.locator('body')).toHaveAttribute('data-completion-resolved', 'true');
  await expect(rows).toHaveText(/choose slownew-input\s*$/);
  await expect(rows).not.toContainText('slow-result');
});

test('local-folder connections use /mnt with folder names and collision suffixes', async ({ page }) => {
  // Use an OPFS-backed picker fixture; this tests mapping without native grants.
  await page.addInitScript(() => {
    Object.defineProperty(window, 'showDirectoryPicker', {
      configurable: true,
      value: async (options: { mode: string }) => {
        if (options.mode !== 'read') throw new Error('Connecting for browsing must request read access only');
        const root = await navigator.storage.getDirectory();
        const folder = await root.getDirectoryHandle('projects', { create: true });
        const file = await folder.getFileHandle('readme.txt', { create: true });
        const writer = await file.createWritable();
        await writer.write('mounted-folder-content'); await writer.close();
        Object.defineProperty(folder, 'queryPermission', { value: async ({ mode }: { mode: string }) => mode === 'readwrite' ? 'prompt' : 'granted' });
        Object.defineProperty(folder, 'requestPermission', { value: () => { throw new Error('Mounting must not request write access'); } });
        return folder;
      },
    });
  });
  await page.reload(); await page.waitForFunction(() => !!window.bt);
  const status = page.getByRole('region', { name: 'Browser filesystem' }).getByRole('status');
  await page.getByRole('button', { name: 'Connect local folder', exact: true }).click();
  await expect(status).toContainText('Connected projects at /mnt/projects.');
  const root = page.locator('[data-browser-terminal]');
  const rows = root.locator('[data-active="true"] .xterm-rows:visible');
  const input = root.locator('[data-active="true"] .xterm-helper-textarea:visible');
  await expect(rows).toHaveText(/\/mnt\/projects ❯\s*$/);
  expect(await page.evaluate(async () => (await window.bt.run('cat readme.txt')).value)).toBe('mounted-folder-content');
  await page.evaluate(() => window.bt.run('edit readme.txt'));
  const editor = page.getByRole('dialog', { name: 'Edit /mnt/projects/readme.txt', exact: true });
  await expect(editor.getByRole('button', { name: 'Enable writes', exact: true })).toBeVisible();
  await editor.getByRole('button', { name: 'Close', exact: true }).click();
  await input.pressSequentially('cd ..'); await input.press('Enter');
  await expect(rows).toHaveText(/\/mnt ❯\s*$/);
  await input.pressSequentially('cat /mnt/proj'); await input.press('Tab');
  await expect(rows).toHaveText(/cat \/mnt\/projects\/\s*$/);
  await input.pressSequentially('read'); await input.press('Tab');
  await expect(rows).toHaveText(/cat \/mnt\/projects\/readme.txt\s*$/);
  await input.press('Enter'); await expect(rows).toContainText('mounted-folder-content');
  await page.getByRole('button', { name: 'Connect local folder', exact: true }).click();
  await expect(status).toContainText('Connected projects at /mnt/projects-2.');
  expect(await page.evaluate(async () => (await window.bt.run('ls /mnt | length')).value)).toBe(2);
  await page.getByRole('button', { name: 'Browser files', exact: true }).click();
  await expect(rows).toHaveText(/\/scratch ❯\s*$/);
});

test('folder connection shows pending and cancellation states without changing mounts', async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(window, 'showDirectoryPicker', { configurable: true, value: () => new Promise((_resolve, reject) => {
      document.addEventListener('cancel-test-picker', () => reject(new DOMException('Cancelled', 'AbortError')), { once: true });
    }) });
  });
  await page.reload(); await page.waitForFunction(() => !!window.bt);
  const button = page.getByRole('button', { name: 'Connect local folder', exact: true });
  const status = page.getByRole('region', { name: 'Browser filesystem' }).getByRole('status');
  await button.click();
  await expect(button).toBeDisabled();
  await expect(status).toContainText('Choose a local folder');
  await page.evaluate(() => document.dispatchEvent(new Event('cancel-test-picker')));
  await expect(status).toContainText('No folder connected');
  await expect(status).toContainText('after selection if access is denied');
  await expect(status).toContainText('no permission prompt appeared');
  await expect(status).toContainText('external Chrome or Edge');
  await expect(status).toContainText('Browser files remain available at /scratch');
  await expect(status).toContainText('AbortError: Cancelled');
  await expect(button).toBeEnabled();
  expect(await page.evaluate(async () => (await window.bt.run('pwd')).value)).toBe('/scratch');
  expect(await page.evaluate(async () => (await window.bt.run('ls /mnt | length')).value)).toBe(0);
});

test('synchronous picker failures are reported and allow retry', async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(window, 'showDirectoryPicker', { configurable: true, value: () => { throw new DOMException('Folder access blocked', 'NotAllowedError'); } });
  });
  await page.reload(); await page.waitForFunction(() => !!window.bt);
  const button = page.getByRole('button', { name: 'Connect local folder', exact: true });
  await button.click();
  await expect(page.getByRole('region', { name: 'Browser filesystem' }).getByRole('status')).toContainText('Could not connect a local folder: NotAllowedError: Folder access blocked');
  await expect(button).toBeEnabled();
  expect(await page.evaluate(async () => (await window.bt.run('ls /mnt | length')).value)).toBe(0);
});
