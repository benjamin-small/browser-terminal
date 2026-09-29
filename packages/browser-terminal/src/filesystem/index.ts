import type { RedirectHandler, Value } from '../types.js';
import type { ArgumentCompletionContext, CompletionItem } from '../completion.js';
import type { ByteReader, DirectoryHandle, EditorDocument, Entry, FileHandle, FilesystemHost, FilesystemOptions, FsContext, WriteTransaction } from './types.js';
import { checkRange, decodeText, integer, normalizePath, sameBytes } from './paths.js';
import { openTransaction, WriteGate } from './io.js';
export type * from './types.js';
export { normalizePath } from './paths.js';
export { openBlockDevice } from './devices.js';

interface Mount { name: string; root: DirectoryHandle; writable: boolean; controller: AbortController }
interface Location { path: string; parts: string[]; relativeParts: string[]; mount?: Mount; signal: AbortSignal; check(): void }
interface PermissionHandle {
  queryPermission?(options: { mode: 'readwrite' }): Promise<PermissionState>;
  requestPermission?(options: { mode: 'readwrite' }): Promise<PermissionState>;
}
const installed = new WeakMap<FilesystemHost, BrowserFilesystem>();
const encoder = new TextEncoder();

async function* batches(entries: AsyncIterable<Entry>): AsyncGenerator<Entry[]> {
  // Mark even empty/single-entry results as collections across the WASM boundary.
  yield [];
  for await (const entry of entries) yield [entry];
}

export class BrowserFilesystem {
  private readonly mounts = new Map<string, Mount>();
  private readonly directories = new Map<number, string>();
  private readonly sessions = new Map<number, AbortController>();
  private readonly cdQueues = new Map<number, Promise<void>>();
  private readonly controller = new AbortController();
  private readonly writes = new WriteGate();
  private readonly cleanup: Array<() => void> = [];
  readonly maxReadBytes: number;
  readonly maxEditorBytes: number;
  readonly chunkSize: number;

  constructor(private readonly options: FilesystemOptions = {}) {
    this.maxReadBytes = integer(options.maxReadBytes ?? 8 * 1024 * 1024, 'maxReadBytes');
    this.maxEditorBytes = integer(options.maxEditorBytes ?? 2 * 1024 * 1024, 'maxEditorBytes');
    this.chunkSize = integer(options.chunkSize ?? 64 * 1024, 'chunkSize');
    if (!this.chunkSize || !this.maxReadBytes || !this.maxEditorBytes) throw new Error('Filesystem limits must be positive');
  }

  /** Installs commands only. Redirects and permission prompts remain host-owned. */
  install(host: FilesystemHost): this {
    this.live();
    if (installed.has(host)) throw new Error('A filesystem is already installed on this terminal');
    if (this.cleanup.length) throw new Error('This filesystem is already installed');
    const add = (spec: Parameters<FilesystemHost['registerOwnedCommand']>[0], fn: Parameters<FilesystemHost['registerOwnedCommand']>[1]) => {
      this.cleanup.push(host.registerOwnedCommand(spec, fn));
    };
    try {
      add({ name: 'pwd', summary: 'Print this session’s virtual working directory' }, (_, _input, ctx) => this.pwd(ctx.session));
      add({ name: 'cd', summary: 'Change directory (use cd path; ls to sequence)', optional: [{ name: 'path', shape: 'str' }] },
        ({ positionals }, _, ctx) => this.cd(String(positionals[0] ?? '/'), ctx));
      add({ name: 'ls', summary: 'List directory entries as records', optional: [{ name: 'path', shape: 'str' }], flags: [{ long: 'long', short: 'l' }] },
        ({ positionals, flags }, _, ctx) => batches(this.list(String(positionals[0] ?? '.'), ctx, Boolean(flags.long))));
      add({ name: 'cat', summary: 'Read a bounded UTF-8 text file', required: [{ name: 'path', shape: 'str' }] },
        ({ positionals }, _, ctx) => this.readText(String(positionals[0]), ctx));
      add({ name: 'read-bytes', summary: 'Read a bounded byte range', required: [{ name: 'path', shape: 'str' }], flags: [{ long: 'offset', shape: 'int' }, { long: 'length', shape: 'int' }] },
        ({ positionals, flags }, _, ctx) => this.readBytes(String(positionals[0]), ctx, Number(flags.offset ?? 0), flags.length === undefined ? undefined : Number(flags.length)));
      add({ name: 'edit', summary: 'Open an existing UTF-8 file in the host editor', required: [{ name: 'path', shape: 'str' }] },
        async ({ positionals }, _, ctx) => {
          if (!this.options.editor) throw new Error('No editor installed; provide the filesystem editor option');
          const document = await this.document(String(positionals[0]), ctx);
          ctx.signal.throwIfAborted();
          await this.options.editor(document);
        });
      this.cleanup.push(host.onLifecycle(event => {
        if (event.type === 'dispose') this.dispose();
        else {
          this.sessions.get(event.session)?.abort(new Error('Session closed'));
          this.sessions.delete(event.session);
          this.directories.delete(event.session); this.cdQueues.delete(event.session);
        }
      }));
      if (host.addCompletionProvider) this.cleanup.push(host.addCompletionProvider(context => this.completePath(context)));
      installed.set(host, this);
      this.cleanup.push(() => { installed.delete(host); });
    } catch (error) {
      for (const remove of this.cleanup.splice(0).reverse()) remove();
      throw error;
    }
    return this;
  }

  private live(): void { this.controller.signal.throwIfAborted(); }
  /** Filesystem arguments and redirect targets; never opens a picker or requests access. */
  async completePath(context: ArgumentCompletionContext): Promise<CompletionItem[]> {
    const { command, prefix, signal, session } = context;
    if (context.kind !== 'redirect' && (context.flag || context.argumentIndex !== 0 || !['cat', 'cd', 'ls', 'edit', 'read-bytes'].includes(command))) return [];
    const cwd = this.pwd(session);
    const slash = prefix.lastIndexOf('/');
    const parent = slash < 0 ? '' : prefix.slice(0, slash + 1);
    const leaf = prefix.slice(slash + 1);
    const entries: CompletionItem[] = [];
    let scanned = 0;
    for await (const entry of this.list(parent || '.', { session, signal })) {
      signal.throwIfAborted();
      if (++scanned > 5000 || entries.length >= 200) break;
      if (!entry.name.startsWith(leaf) || (command === 'cd' && context.kind !== 'redirect' && entry.kind !== 'directory')) continue;
      const directory = entry.kind === 'directory';
      entries.push({ value: parent + entry.name + (directory ? '/' : ''), directory });
    }
    signal.throwIfAborted();
    return this.pwd(session) === cwd ? entries : [];
  }
  mount(name: string, root: DirectoryHandle | FileSystemDirectoryHandle, options: { writable?: boolean } = {}): void {
    this.live();
    name = name.replace(/^\//, '');
    if (!name || name.split('/').some(part => !part || part === '.' || part === '..' || part.includes('\0')) || name === 'mnt' || (name.split('/')[0] === 'dev' && this.options.devices)) throw new Error('Invalid or reserved mount name');
    if (this.mounts.has(name)) throw new Error(`Mount already exists: /${name}`);
    if ([...this.mounts.keys()].some(existing => existing.startsWith(`${name}/`) || name.startsWith(`${existing}/`))) throw new Error('Mount paths cannot overlap');
    if (root.kind !== 'directory' || typeof (root as DirectoryHandle).values !== 'function') throw new Error('Expected a supported directory handle');
    this.mounts.set(name, { name, root: root as DirectoryHandle, writable: options.writable ?? false, controller: new AbortController() });
    this.options.onDirectoryChange?.();
  }
  /** Map a selected local directory under /mnt, returning its full virtual path. */
  mountLocal(root: DirectoryHandle | FileSystemDirectoryHandle, options: { writable?: boolean; name?: string } = {}): string {
    this.live();
    const base = options.name ?? root.name;
    if (!base || base === '.' || base === '..' || /[/\0]/.test(base)) throw new Error('Invalid local mount name');
    let name = `mnt/${base}`, suffix = 2;
    while ([...this.mounts.keys()].some(existing => existing === name || existing.startsWith(`${name}/`))) name = `mnt/${base}-${suffix++}`;
    this.mount(name, root, options);
    return `/${name}`;
  }
  async mountScratch(name = 'scratch', options: { writable?: boolean } = {}): Promise<void> {
    if (!navigator.storage?.getDirectory) throw new Error('Browser-private storage is unavailable');
    this.mount(name, await navigator.storage.getDirectory() as unknown as DirectoryHandle, options);
  }
  unmount(name: string): void {
    this.live();
    name = name.replace(/^\//, '');
    const mount = this.mounts.get(name);
    if (!mount) throw new Error(`Unknown mount: /${name}`);
    mount.controller.abort(new Error(`Unmounted /${name}`));
    this.mounts.delete(name);
    for (const [session, cwd] of this.directories) {
      if (!this.mountAt(cwd) && !this.virtualDirectory(cwd) && !(cwd === '/dev' && this.options.devices)) this.directories.set(session, '/');
    }
    this.options.onDirectoryChange?.();
  }
  pwd(session: number): string {
    this.live();
    const initial = normalizePath(this.options.initialDirectory ?? '/');
    const mounted = this.virtualDirectory(initial) || !!this.mountAt(initial);
    return this.directories.get(session) ?? (mounted ? initial : '/');
  }
  private mountAt(path: string): Mount | undefined {
    return [...this.mounts.values()].find(mount => path === `/${mount.name}` || path.startsWith(`/${mount.name}/`));
  }
  private virtualDirectory(path: string): boolean {
    return path === '/' || path === '/mnt' || [...this.mounts.keys()].some(name => `/${name}`.startsWith(`${path}/`));
  }
  private locate(path: string, ctx: FsContext): Location {
    this.live();
    const normalized = normalizePath(path, this.pwd(ctx.session));
    const parts = normalized.split('/').filter(Boolean);
    const mount = this.mountAt(normalized);
    if (!mount && !this.virtualDirectory(normalized) && !(parts[0] === 'dev' && this.options.devices)) throw new Error(`Unknown mount: ${normalized}`);
    let session = this.sessions.get(ctx.session);
    if (!session) { session = new AbortController(); this.sessions.set(ctx.session, session); }
    const signal = AbortSignal.any([this.controller.signal, session.signal, ...(mount ? [mount.controller.signal] : []), ...(ctx.signal ? [ctx.signal] : [])]);
    const check = () => { signal.throwIfAborted(); };
    check();
    return { path: normalized, parts, relativeParts: mount ? parts.slice(mount.name.split('/').length) : [], mount, signal, check };
  }
  private async directory(location: Location, parts = location.relativeParts): Promise<DirectoryHandle> {
    if (!location.mount) throw new Error(`Not a filesystem directory: ${location.path}`);
    let directory = location.mount.root;
    for (const name of parts) { location.check(); directory = await directory.getDirectoryHandle(name); }
    location.check();
    return directory;
  }
  private async file(location: Location, create = false): Promise<FileHandle> {
    if (!location.mount || !location.relativeParts.length) throw new Error(`Not a file: ${location.path}`);
    const directory = await this.directory(location, location.relativeParts.slice(0, -1));
    const handle = await directory.getFileHandle(location.relativeParts.at(-1)!, { create });
    location.check();
    return handle;
  }
  async cd(path: string, ctx: FsContext): Promise<void> {
    // Snapshot before queuing: concurrent commands do not silently retarget one another.
    const location = this.locate(path, ctx);
    const prior = this.cdQueues.get(ctx.session) ?? Promise.resolve();
    const result = prior.catch(() => {}).then(async () => {
      location.check();
      if (location.mount) await this.directory(location);
      else if (!this.virtualDirectory(location.path) && location.path !== '/dev') throw new Error(`Not a directory: ${location.path}`);
      location.check();
      this.directories.set(ctx.session, location.path);
      this.options.onDirectoryChange?.();
    });
    this.cdQueues.set(ctx.session, result);
    try { await result; } finally { if (this.cdQueues.get(ctx.session) === result) this.cdQueues.delete(ctx.session); }
  }
  list(path: string, ctx: FsContext, long = false): AsyncGenerator<Entry> {
    return this.listAt(this.locate(path, ctx), long);
  }
  private async *listAt(location: Location, long: boolean): AsyncGenerator<Entry> {
    if (!location.mount && this.virtualDirectory(location.path)) {
      const prefix = location.path === '/' ? '/' : `${location.path}/`;
      const paths = ['/mnt', ...[...this.mounts.keys()].map(name => `/${name}`), ...(this.options.devices ? ['/dev'] : [])];
      const children = new Set(paths.filter(path => path.startsWith(prefix)).map(path => path.slice(prefix.length).split('/')[0]!).filter(Boolean));
      for (const name of [...children].sort()) {
        location.check(); yield { name, kind: 'directory', path: `${prefix}${name}` };
      }
    } else if (location.path === '/dev' && this.options.devices) {
      for (const name of ['null', 'zero']) yield { name, kind: 'device', path: `/dev/${name}` };
    } else {
      const directory = await this.directory(location);
      // Serial metadata reads bound concurrency to one and preserve streaming/backpressure.
      for await (const handle of directory.values()) {
        location.check();
        const entry: Entry = { name: handle.name, kind: handle.kind, path: `${location.path}/${handle.name}` };
        if (long) {
          const file = handle.kind === 'file' ? await handle.getFile() : null;
          entry.size = file?.size ?? null;
          entry.modified = file?.lastModified ?? null;
        }
        location.check(); yield entry;
      }
    }
  }
  async openReader(path: string, ctx: FsContext): Promise<ByteReader> {
    const location = this.locate(path, ctx);
    const device = location.parts[0] === 'dev' && this.options.devices;
    if (device && location.path !== '/dev/null' && location.path !== '/dev/zero') throw new Error('Unknown byte device');
    const file = device ? null : await (await this.file(location)).getFile();
    location.check();
    const size = file ? file.size : location.path === '/dev/null' ? 0 : null;
    const max = this.maxReadBytes;
    const defaultChunk = this.chunkSize;
    const readAt = async (offset: number, length: number) => {
      checkRange(offset, length); location.check();
      if (length > max) throw new Error(`Read exceeds ${max} bytes`);
      const bytes = file ? new Uint8Array(await file.slice(offset, offset + length).arrayBuffer())
        : new Uint8Array(size === 0 ? 0 : length);
      location.check(); return bytes;
    };
    return {
      size,
      capabilities: Object.freeze({ read: true, write: false, seek: !!file, truncate: false, stream: true }),
      readAt,
      async *chunks(options = {}) {
        let offset = options.offset ?? 0;
        let remaining = options.length ?? (size === null ? NaN : Math.max(0, size - offset));
        checkRange(offset, remaining);
        const chunkSize = options.chunkSize ?? defaultChunk;
        integer(chunkSize, 'chunkSize');
        if (!chunkSize || chunkSize > max) throw new Error('Invalid chunk size');
        while (remaining) {
          const bytes = await readAt(offset, Math.min(remaining, chunkSize));
          if (!bytes.length) break;
          yield bytes; offset += bytes.length; remaining -= bytes.length;
        }
      },
    };
  }
  async readBytes(path: string, ctx: FsContext, offset = 0, length?: number): Promise<Uint8Array> {
    const reader = await this.openReader(path, ctx);
    if (length === undefined && reader.size === null) throw new Error('Device reads require an explicit length');
    return reader.readAt(offset, length ?? Math.max(0, reader.size! - offset));
  }
  async readText(path: string, ctx: FsContext): Promise<string> {
    const location = this.locate(path, ctx);
    if (location.parts[0] === 'dev' && this.options.devices) throw new Error('Use bounded read-bytes for devices');
    return decodeText(await this.readBytes(location.path, ctx));
  }
  private async allowWrite(location: Location): Promise<void> {
    location.check();
    if (!location.mount?.writable) throw new Error('Mount is read-only; enable writes in the host');
    const permissions = location.mount.root as PermissionHandle;
    if (permissions.queryPermission && await permissions.queryPermission({ mode: 'readwrite' }) !== 'granted') {
      throw new Error('Write permission required; use the host’s Enable writes button');
    }
    location.check();
  }
  /** Call directly in a click handler; this never changes the mount's host policy. */
  async requestWritePermission(name: string): Promise<boolean> {
    this.live();
    name = name.replace(/^\//, '');
    const mount = this.mounts.get(name);
    if (!mount?.writable) throw new Error('Mount is read-only; enable writes in the host');
    const permissions = mount.root as PermissionHandle;
    const granted = permissions.requestPermission ? await permissions.requestPermission({ mode: 'readwrite' }) === 'granted' : true;
    mount.controller.signal.throwIfAborted(); this.live();
    return granted;
  }
  async beginWrite(path: string, ctx: FsContext, options: { create?: boolean; expected?: Uint8Array } = {}): Promise<WriteTransaction> {
    const location = this.locate(path, ctx);
    const expected = options.expected === undefined ? undefined : new Uint8Array(options.expected);
    await this.allowWrite(location);
    const release = await this.writes.acquire(location.signal);
    try {
      await this.allowWrite(location);
      const handle = await this.file(location, options.create ?? false);
      if (expected) {
        const file = await handle.getFile();
        if (file.size !== expected.length || !sameBytes(new Uint8Array(await file.arrayBuffer()), expected)) throw new Error('File changed externally; reload or export your unsaved text');
      }
      return await openTransaction(handle, location.signal, location.check, release, this.maxReadBytes, () => this.allowWrite(location));
    } catch (error) { release(); throw error; }
  }
  async write(path: string, bytes: Uint8Array, ctx: FsContext, options: { append?: boolean; expected?: Uint8Array } = {}): Promise<void> {
    if (bytes.length > this.maxReadBytes) throw new Error(`Write exceeds ${this.maxReadBytes} bytes`);
    const copy = new Uint8Array(bytes);
    const location = this.locate(path, ctx);
    if (location.path === '/dev/null' && this.options.devices) return;
    const transaction = await this.beginWrite(location.path, { ...ctx, signal: location.signal }, { create: options.expected === undefined, expected: options.expected });
    try {
      if (!options.append) await transaction.truncate(0);
      await transaction.writeAt(options.append ? transaction.size : 0, copy);
      await transaction.commit();
    } catch (error) { await transaction.abort().catch(() => {}); throw error; }
  }
  createRedirectHandler(options: { binary?: boolean } = {}): RedirectHandler {
    const pathFor = (target: string, ctx: FsContext) => {
      const location = this.locate(target, ctx);
      if (location.parts[0] === 'dev' && this.options.devices) throw new Error('Device redirection is unsupported; use bounded byte I/O');
      return location.path;
    };
    return {
      read: (target, ctx) => options.binary ? this.readBytes(pathFor(target, ctx), ctx) : this.readText(pathFor(target, ctx), ctx),
      write: (target, value: Value, ctx) => {
        if (typeof value !== 'string' && !(value instanceof Uint8Array)) throw new Error('File redirects require text or bytes; serialize structured values with to json');
        return this.write(pathFor(target, ctx), typeof value === 'string' ? encoder.encode(value) : value, ctx, { append: ctx.append });
      },
    };
  }
  async document(path: string, ctx: FsContext): Promise<EditorDocument> {
    const initial = this.locate(path, ctx);
    await this.file(initial); initial.check();
    // The editor outlives the opening command, but never its original mount.
    const location = this.locate(initial.path, { session: ctx.session });
    const context = { session: ctx.session, signal: location.signal };
    return {
      path: location.path,
      writable: location.mount?.writable ?? false,
      signal: location.signal,
      read: async () => {
        location.check();
        const reader = await this.openReader(location.path, context);
        if (reader.size === null || reader.size > this.maxEditorBytes) throw new Error(`Editor limit is ${this.maxEditorBytes} bytes`);
        return reader.readAt(0, reader.size);
      },
      save: async (bytes, original) => {
        location.check();
        if (bytes.length > this.maxEditorBytes) throw new Error(`Editor limit is ${this.maxEditorBytes} bytes`);
        await this.write(location.path, bytes, context, { expected: original });
      },
      writePermission: async () => {
        location.check();
        if (!location.mount?.writable) return 'denied';
        const permissions = location.mount.root as PermissionHandle;
        const state = permissions.queryPermission ? await permissions.queryPermission({ mode: 'readwrite' }) : 'granted';
        location.check();
        return state;
      },
      requestWritePermission: () => { location.check(); return this.requestWritePermission(location.mount!.name); },
    };
  }
  dispose(): void {
    if (this.controller.signal.aborted) return;
    this.controller.abort(new Error('Filesystem disposed'));
    for (const remove of this.cleanup.splice(0).reverse()) remove();
    this.mounts.clear(); this.directories.clear(); this.cdQueues.clear(); this.sessions.clear();
  }
}

export function installFilesystem(host: FilesystemHost, options: FilesystemOptions = {}): BrowserFilesystem {
  return new BrowserFilesystem(options).install(host);
}
