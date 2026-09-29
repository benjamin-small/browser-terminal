import { test, expect } from '@playwright/test';
import type { BrowserTerminal } from '@benjamin-small/browser-terminal';

test.beforeEach(async ({ page }) => {
  await page.goto('/');
  await page.waitForFunction(() => !!window.bt && !!window.filesystem);
  await expect(page.getByRole('region', { name: 'Browser filesystem' }).getByRole('status')).toContainText('Browser files ready');
});

test('default creation mounts writable OPFS, commands, editor and session prompts', async ({ page }) => {
  const result = await page.evaluate(async () => {
    const Terminal = window.bt.constructor as typeof BrowserTerminal;
    window.bt.dispose();
    const bt = window.bt = await Terminal.create();
    const pwd = (await bt.run('pwd')).value;
    await bt.run('echo default-files > default-startup.txt');
    const text = (await bt.run('cat default-startup.txt')).value;
    const bytes = (await bt.run('read-bytes default-startup.txt')).value as Uint8Array;
    const listing = (await bt.run("ls | filter {|e| $e.name == 'default-startup.txt'} | length")).value;
    await bt.run('session new defaults');
    const nextPwd = (await bt.run('pwd')).value;
    await bt.run('edit default-startup.txt');
    return { mounted: !!bt.filesystem, pwd, text, bytes: [...bytes], listing, nextPwd };
  });
  expect(result).toEqual({ mounted: true, pwd: '/scratch', text: 'default-files', bytes: [...new TextEncoder().encode('default-files')], listing: 1, nextPwd: '/scratch' });
  await expect(page.getByRole('dialog', { name: 'Edit /scratch/default-startup.txt', exact: true })).toBeVisible();
  const persisted = await page.evaluate(async () => {
    const Terminal = window.bt.constructor as typeof BrowserTerminal;
    window.bt.dispose();
    const bt = window.bt = await Terminal.create();
    const text = (await bt.run('cat default-startup.txt')).value;
    await bt.run('cd /');
    return { text, pwd: (await bt.run('pwd')).value };
  });
  expect(persisted).toEqual({ text: 'default-files', pwd: '/' });
  await expect(page.getByRole('dialog', { name: 'Edit /scratch/default-startup.txt', exact: true })).toHaveCount(0);
});

for (const unavailable of ['missing', 'rejected'] as const) {
  test(`OPFS ${unavailable} warns in terminal and console and keeps shell usable`, async ({ page }) => {
    const warnings: string[] = [];
    page.on('console', message => { if (message.type() === 'warning') warnings.push(message.text()); });
    const result = await page.evaluate(async unavailable => {
      const Terminal = window.bt.constructor as typeof BrowserTerminal;
      window.bt.dispose();
      Object.defineProperty(navigator.storage, 'getDirectory', { configurable: true, value: unavailable === 'missing' ? undefined : async () => { throw new DOMException('Storage denied', 'SecurityError'); } });
      const bt = window.bt = await Terminal.create();
      let missing = false;
      try { await bt.run('pwd'); } catch (error) { missing = String(error).includes('unknown command'); }
      return { mounted: !!bt.filesystem, missing, echo: (await bt.run('echo works')).value };
    }, unavailable);
    expect(result).toEqual({ mounted: false, missing: true, echo: 'works' });
    expect(warnings.filter(text => text.includes('OPFS is unavailable'))).toHaveLength(1);
    await expect(page.locator('[data-browser-terminal] .xterm-screen')).toContainText('OPFS is unavailable');
  });
}

test('explicit opt-out does not access storage or register filesystem commands', async ({ page }) => {
  const warnings: string[] = [];
  page.on('console', message => { if (message.type() === 'warning') warnings.push(message.text()); });
  const result = await page.evaluate(async () => {
    const Terminal = window.bt.constructor as typeof BrowserTerminal;
    window.bt.dispose();
    let calls = 0;
    Object.defineProperty(navigator.storage, 'getDirectory', { configurable: true, value: async () => { calls++; throw new Error('Unexpected storage access'); } });
    const bt = window.bt = await Terminal.create({ filesystem: false });
    let missing = false;
    try { await bt.run('ls'); } catch (error) { missing = String(error).includes('unknown command'); }
    return { mounted: !!bt.filesystem, calls, missing };
  });
  expect(result).toEqual({ mounted: false, calls: 0, missing: true });
  expect(warnings.filter(text => text.includes('OPFS'))).toHaveLength(0);
});
