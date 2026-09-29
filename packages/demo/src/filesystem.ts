import type { BrowserTerminal } from '@benjamin-small/browser-terminal';
import { installFilesystem, type BrowserFilesystem } from '@benjamin-small/browser-terminal/filesystem';
import { createTextEditor } from '@benjamin-small/browser-terminal/filesystem/editor';

declare global { interface Window { filesystem: BrowserFilesystem } }

/** User gestures own picker/permission prompts; terminal commands never prompt. */
export function filesystemDemo(bt: BrowserTerminal): void {
  const editor = createTextEditor();
  const filesystem = installFilesystem(bt, { editor: editor.open, devices: true });
  window.filesystem = filesystem;
  const section = document.createElement('section');
  section.setAttribute('aria-label', 'Browser filesystem');
  const heading = document.createElement('h2'); heading.textContent = 'Files in your browser';
  const description = document.createElement('p');
  description.textContent = 'Connect a folder or open private scratch storage, then try ls, cd, cat, and edit. Writing needs your permission.';
  const status = document.createElement('p'); status.setAttribute('role', 'status');
  const example = document.createElement('pre'); example.textContent = 'ls /\ncd /scratch; ls\nedit welcome.txt\nread-bytes /dev/zero --length 16';
  const controls = document.createElement('div'); controls.style.cssText = 'display:flex;gap:8px;flex-wrap:wrap';
  section.append(heading, description, controls, status, example);
  document.querySelector('main')!.append(section);
  const report = (error: unknown) => { status.textContent = error instanceof Error ? error.message : String(error); };
  const button = (label: string) => { const el = document.createElement('button'); el.textContent = label; el.style.cssText = 'padding:8px;font:inherit'; controls.append(el); return el; };
  const connect = button('Connect folder');
  const scratch = button('Open scratch');
  const writes = button('Enable local writes'); writes.disabled = true;
  const redirects = button('Enable file redirects');
  const picker = (window as unknown as { showDirectoryPicker?: (options: { mode: 'read' }) => Promise<FileSystemDirectoryHandle> }).showDirectoryPicker;
  if (!picker || !window.isSecureContext) {
    connect.disabled = true;
    status.textContent = 'Local folder access is unavailable here. Try private scratch storage.';
  }
  let nextMount = 0;
  let latestMount: string | undefined;
  connect.addEventListener('click', () => {
    const session = bt.snapshot!.sessions.find(item => item.active)!.id;
    // Invoke before any await so native picker activation is preserved.
    void picker!.call(window, { mode: 'read' }).then(async handle => {
      const name = nextMount++ ? `local${nextMount}` : 'local';
      filesystem.mount(name, handle, { writable: true }); latestMount = name; writes.disabled = false;
      await filesystem.cd(`/${name}`, { session });
      status.textContent = `Connected ${handle.name} at /${name}. Try ls or edit filename.`;
    }).catch(report);
  });
  scratch.addEventListener('click', () => {
    const session = bt.snapshot!.sessions.find(item => item.active)!.id;
    scratch.disabled = true;
    void (async () => {
      const root = await navigator.storage.getDirectory();
      const dir = await root.getDirectoryHandle('browser-terminal-demo', { create: true });
      try { await dir.getFileHandle('welcome.txt'); }
      catch (error) {
        if (!(error instanceof DOMException) || error.name !== 'NotFoundError') throw error;
        const handle = await dir.getFileHandle('welcome.txt', { create: true });
        const writer = await handle.createWritable(); await writer.write('Welcome to browser-terminal.\nEdit this file and choose Save.\n'); await writer.close();
      }
      filesystem.mount('scratch', dir, { writable: true });
      await filesystem.cd('/scratch', { session });
      status.textContent = 'Scratch storage mounted at /scratch. Try edit welcome.txt.';
    })().catch(error => { scratch.disabled = false; report(error); });
  });
  if (!navigator.storage?.getDirectory) scratch.disabled = true;
  writes.addEventListener('click', () => {
    if (latestMount) void filesystem.requestWritePermission(latestMount).then(granted => {
      status.textContent = granted ? 'Local writes enabled.' : 'Write access denied.';
    }).catch(report);
  });
  redirects.addEventListener('click', () => {
    bt.setRedirectHandler(filesystem.createRedirectHandler()); redirects.disabled = true;
    status.textContent = 'File redirects enabled. Try echo hello > /scratch/hello.txt';
  });
  bt.onLifecycle(event => {
    if (event.type === 'dispose') {
      connect.disabled = scratch.disabled = writes.disabled = redirects.disabled = true;
      // Editors remain available for exporting dirty buffers after disconnection.
    }
  });
}
