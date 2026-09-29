import { test } from 'node:test';
import assert from 'node:assert/strict';
import { BrowserFilesystem, normalizePath, openBlockDevice } from '../dist/filesystem/index.js';
import { textModel } from '../dist/filesystem/editor.js';

const encode = value => new TextEncoder().encode(value);
const decode = value => new TextDecoder().decode(value);
const ctx = { session: 1 };
class MemoryFile {
  kind = 'file';
  bytes;
  failClose = false;
  failWrite = false;
  closes = 0;
  aborts = 0;
  constructor(name, value = '') { this.name = name; this.bytes = typeof value === 'string' ? encode(value) : value.slice(); }
  async getFile() { return new File([this.bytes], this.name, { lastModified: 1 }); }
  async isSameEntry(other) { return this === other; }
  async createWritable({ keepExistingData } = {}) {
    let data = keepExistingData ? this.bytes.slice() : new Uint8Array();
    let closed = false;
    const resize = size => { const next = new Uint8Array(size); next.set(data.subarray(0, size)); data = next; };
    return {
      write: async ({ position, data: bytes }) => {
        assert.equal(closed, false);
        if (this.failWrite) throw new Error('quota exceeded');
        if (position + bytes.length > data.length) resize(position + bytes.length);
        data.set(bytes, position);
      },
      truncate: async size => { assert.equal(closed, false); resize(size); },
      close: async () => { if (this.failClose) throw new Error('close failed'); this.closes++; this.bytes = data; closed = true; },
      abort: async () => { this.aborts++; closed = true; },
    };
  }
}
class MemoryDirectory {
  kind = 'directory';
  entries = new Map();
  permission = 'granted';
  constructor(name = 'root') { this.name = name; }
  async queryPermission() { return this.permission; }
  async requestPermission() { return this.permission; }
  async getDirectoryHandle(name) {
    const entry = this.entries.get(name);
    if (!entry || entry.kind !== 'directory') throw new Error('Not a directory');
    return entry;
  }
  async getFileHandle(name, { create = false } = {}) {
    let entry = this.entries.get(name);
    if (!entry && create) { entry = new MemoryFile(name); this.entries.set(name, entry); }
    if (!entry || entry.kind !== 'file') throw new Error('Not a file');
    return entry;
  }
  async *values() { yield* this.entries.values(); }
  file(name, value = '') { const file = new MemoryFile(name, value); this.entries.set(name, file); return file; }
  directory(name) { const dir = new MemoryDirectory(name); this.entries.set(name, dir); return dir; }
}
function fixture(options = {}) {
  const root = new MemoryDirectory(); root.file('hello.txt', 'hello'); root.directory('src').file('日本語 space.txt', 'world');
  const fs = new BrowserFilesystem(options); fs.mount('local', root, { writable: true });
  return { fs, root };
}
async function collect(stream) { const result = []; for await (const item of stream) result.push(item); return result; }

 test('paths remain in the virtual namespace without decoding names', () => {
  assert.equal(normalizePath('../../../../etc', '/local/src'), '/etc');
  assert.equal(normalizePath('./a//b/../日本語', '/local'), '/local/a/日本語');
  assert.equal(normalizePath('%2e%2e/~', '/local'), '/local/%2e%2e/~');
  assert.throws(() => normalizePath('a\0b'));
});
test('navigation is session scoped and failed cd leaves cwd intact', async () => {
  const { fs } = fixture();
  await fs.cd('/local/src', ctx);
  assert.equal(fs.pwd(1), '/local/src'); assert.equal(fs.pwd(2), '/');
  assert.equal(await fs.readText('日本語 space.txt', ctx), 'world');
  await assert.rejects(fs.cd('missing', ctx)); assert.equal(fs.pwd(1), '/local/src');
  await fs.cd('..', ctx); await fs.cd('..', ctx); assert.equal(fs.pwd(1), '/');
  await assert.rejects(fs.cd('/etc', ctx), /Unknown mount/);
  const list = await collect(fs.list('/local', ctx, true));
  assert.equal(list.find(x => x.name === 'hello.txt').size, 5);
  assert.equal(list.find(x => x.name === 'src').size, null);
});
test('reads snapshot cwd and abort on unmount, never retarget after remount', async () => {
  const { fs, root } = fixture(); await fs.cd('/local', ctx);
  const pending = fs.readText('hello.txt', ctx); await fs.cd('/local/src', ctx);
  assert.equal(await pending, 'hello');
  const reader = await fs.openReader('/local/hello.txt', ctx);
  fs.unmount('local'); fs.mount('local', root);
  assert.equal(fs.pwd(1), '/'); await assert.rejects(reader.readAt(0, 2), /Unmounted/);
});
test('byte ranges, snapshots, bounds, malformed UTF-8, and chunked reads', async () => {
  const { fs, root } = fixture({ maxReadBytes: 4, chunkSize: 2 });
  await assert.rejects(fs.readBytes('/local/hello.txt', ctx), /exceeds/);
  assert.equal(decode(await fs.readBytes('/local/hello.txt', ctx, 3, 4)), 'lo');
  assert.equal((await fs.readBytes('/local/hello.txt', ctx, 9, 1)).length, 0);
  await assert.rejects(fs.readBytes('/local/hello.txt', ctx, -1, 1));
  const reader = await fs.openReader('/local/hello.txt', ctx);
  root.entries.get('hello.txt').bytes = encode('other');
  assert.equal((await collect(reader.chunks())).map(decode).join(''), 'hello');
  root.file('bad', new Uint8Array([255])); await assert.rejects(fs.readText('/local/bad', ctx));
});
test('staged patches preserve untouched bytes; abort discards and close commits', async () => {
  const { fs } = fixture(); const tx = await fs.beginWrite('/local/hello.txt', ctx);
  await tx.writeAt(1, encode('A'));
  assert.equal(await fs.readText('/local/hello.txt', ctx), 'hello');
  await tx.commit(); assert.equal(await fs.readText('/local/hello.txt', ctx), 'hAllo');
  await assert.rejects(tx.commit(), /closed/);
  const abort = await fs.beginWrite('/local/hello.txt', ctx); await abort.truncate(1); await abort.abort();
  assert.equal(await fs.readText('/local/hello.txt', ctx), 'hAllo');
});
test('redirects preserve text/bytes, append and empty writes, reject implicit serialization', async () => {
  const { fs } = fixture(); const redirect = fs.createRedirectHandler();
  const context = { ...ctx, pane: 0, signal: new AbortController().signal, append: false };
  await redirect.write('/local/new', 'A', context);
  await redirect.write('/local/new', new Uint8Array([0, 255]), { ...context, append: true });
  assert.deepEqual(await fs.readBytes('/local/new', ctx), new Uint8Array([65, 0, 255]));
  assert.throws(() => redirect.write('/local/new', [{ x: 1 }], context), /serialize/);
  await redirect.write('/local/new', '', context); assert.equal(await redirect.read('/local/new', context), '');
  await fs.write('/local/new', new Uint8Array([255]), ctx);
  assert.deepEqual(await fs.createRedirectHandler({ binary: true }).read('/local/new', context), new Uint8Array([255]));
});
test('read-only policy, permission revocation, quota and close failures preserve originals', async () => {
  const { fs, root } = fixture(); fs.mount('readonly', root);
  await assert.rejects(fs.write('/readonly/hello.txt', encode('bad'), ctx), /read-only/);
  root.permission = 'prompt'; await assert.rejects(fs.write('/local/hello.txt', encode('bad'), ctx), /permission/);
  root.permission = 'granted'; const file = root.entries.get('hello.txt');
  file.failWrite = true; await assert.rejects(fs.write('/local/hello.txt', encode('bad'), ctx), /quota/);
  file.failWrite = false; file.failClose = true;
  await assert.rejects(fs.write('/local/hello.txt', encode('bad'), ctx), /close failed/);
  assert.equal(decode(file.bytes), 'hello'); assert.equal(file.aborts, 2);
  file.failClose = false; await fs.write('/local/hello.txt', encode('ok'), ctx);
});
test('overlapping mounts serialize append against the same file', async () => {
  const { fs, root } = fixture(); fs.mount('alias', root, { writable: true });
  await Promise.all([fs.write('/local/hello.txt', encode('A'), ctx, { append: true }), fs.write('/alias/hello.txt', encode('B'), ctx, { append: true })]);
  assert.equal(await fs.readText('/local/hello.txt', ctx), 'helloAB');
});
test('cancelled queued writes release their slot and an unmount aborts a transaction', async () => {
  const { fs, root } = fixture(); const tx = await fs.beginWrite('/local/hello.txt', ctx);
  const controller = new AbortController();
  const waiting = fs.beginWrite('/local/hello.txt', { ...ctx, signal: controller.signal });
  controller.abort(new Error('cancelled')); await assert.rejects(waiting, /cancelled/);
  await tx.writeAt(0, encode('bad')); fs.unmount('local');
  await assert.rejects(tx.commit());
  fs.mount('again', root, { writable: true }); await fs.write('/again/hello.txt', encode('recovered'), ctx);
  assert.equal(await fs.readText('/again/hello.txt', ctx), 'recovered');
});
test('editor detects external edits and never recreates a removed file on save', async () => {
  const { fs, root } = fixture(); const doc = await fs.document('/local/hello.txt', ctx); const original = await doc.read();
  await doc.save(encode('edited'), original); assert.equal(await fs.readText('/local/hello.txt', ctx), 'edited');
  await assert.rejects(doc.save(encode('stale'), original), /changed externally/);
  root.entries.delete('hello.txt'); await assert.rejects(doc.save(encode('stale'), original));
  assert.equal(root.entries.has('hello.txt'), false);
});
test('text model preserves UTF-8 BOM and CRLF and marks mixed endings read-only', () => {
  const bytes = new Uint8Array([239, 187, 191, ...encode('a\r\nb\r\n')]);
  const model = textModel(bytes); assert.equal(model.text, 'a\nb\n'); assert.deepEqual(model.encode(model.text), bytes);
  assert.equal(textModel(encode('a\r\nb\n')).mixed, true);
  assert.equal(textModel(encode('a\rb')).mixed, true);
  assert.throws(() => textModel(new Uint8Array([255])));
});
test('editor bounds and mount invalidation', async () => {
  const { fs } = fixture({ maxEditorBytes: 4 }); const doc = await fs.document('/local/hello.txt', ctx);
  await assert.rejects(doc.read(), /Editor limit/); await assert.rejects(doc.save(encode('12345'), encode('hello')), /Editor limit/);
  fs.unmount('local'); await assert.rejects(doc.read(), /Unmounted/);
});
test('devices are opt-in, bounded and cannot enter collecting redirects', async () => {
  const { fs } = fixture({ devices: true, maxReadBytes: 16 });
  assert.deepEqual(await fs.readBytes('/dev/zero', ctx, 0, 8), new Uint8Array(8));
  assert.equal((await fs.readBytes('/dev/null', ctx)).length, 0);
  await fs.write('/dev/null', encode('discard'), ctx);
  await assert.rejects(fs.readBytes('/dev/zero', ctx), /explicit length/);
  await assert.rejects(fs.readText('/dev/zero', ctx), /bounded/);
  await assert.rejects(fs.write('/dev/zero', encode('bad'), ctx));
  assert.throws(() => fs.createRedirectHandler().read('/dev/zero', ctx), /unsupported/);
  await assert.rejects(collect((await fs.openReader('/dev/zero', ctx)).chunks()));
  await assert.rejects(fixture().fs.readBytes('/dev/null', ctx), /Unknown mount/);
});
test('block writes preserve adjacent blocks and reject alignment/capacity changes', async () => {
  const { fs, root } = fixture(); root.file('disk', new Uint8Array(1024).fill(1));
  const disk = await openBlockDevice(fs, '/local/disk', ctx);
  await disk.write(1, new Uint8Array(512).fill(7));
  assert.equal((await disk.read(0))[0], 1); assert.equal((await disk.read(1))[0], 7);
  await assert.rejects(disk.write(0, new Uint8Array(1)), /aligned/);
  await assert.rejects(disk.read(2), /capacity/);
  root.entries.get('disk').bytes = new Uint8Array(512); await assert.rejects(disk.read(0), /size changed/);
});
test('installation rolls back collisions and disposal preserves replacement commands', () => {
  const commands = new Map(); const listeners = new Set();
  const host = {
    registerOwnedCommand(spec, fn) {
      if (commands.has(spec.name)) throw new Error('collision'); commands.set(spec.name, fn);
      return () => { if (commands.get(spec.name) === fn) commands.delete(spec.name); };
    },
    onLifecycle(fn) { listeners.add(fn); return () => listeners.delete(fn); },
  };
  commands.set('cat', 'original'); const failed = new BrowserFilesystem();
  assert.throws(() => failed.install(host), /collision/); assert.deepEqual([...commands.keys()], ['cat']);
  commands.clear(); const fs = new BrowserFilesystem().install(host);
  assert.throws(() => new BrowserFilesystem().install(host), /already installed/);
  commands.set('ls', 'replacement'); fs.dispose(); assert.deepEqual([...commands], [['ls', 'replacement']]); assert.equal(listeners.size, 0);
});

test('permission revocation before commit aborts staged writes', async () => {
  const { fs, root } = fixture();
  const tx = await fs.beginWrite('/local/hello.txt', ctx); await tx.writeAt(0, encode('X'));
  root.permission = 'denied'; await assert.rejects(tx.commit(), /permission/);
  assert.equal(decode(root.entries.get('hello.txt').bytes), 'hello');
});

test('list snapshots cwd before iteration and checks cancellation', async () => {
  const { fs } = fixture(); await fs.cd('/local', ctx);
  const entries = fs.list('.', ctx); await fs.cd('/local/src', ctx);
  assert.equal((await collect(entries)).length, 2);
  const controller = new AbortController();
  const cancelled = fs.list('/local', { ...ctx, signal: controller.signal });
  controller.abort(new Error('cancel list')); await assert.rejects(collect(cancelled), /cancel list/);
});

test('session cleanup invalidates its readers without disconnecting another session', async () => {
  const { fs } = fixture(); let listener;
  fs.install({ registerOwnedCommand: () => () => {}, onLifecycle: fn => { listener = fn; return () => {}; } });
  const first = await fs.openReader('/local/hello.txt', ctx);
  const second = await fs.openReader('/local/hello.txt', { session: 2 });
  listener({ type: 'sessionClosed', session: 1 });
  await assert.rejects(first.readAt(0, 1), /Session closed/);
  assert.equal(decode(await second.readAt(0, 1)), 'h');
  fs.dispose(); await assert.rejects(second.readAt(0, 1), /disposed/);
});
