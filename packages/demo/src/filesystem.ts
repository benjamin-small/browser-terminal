import type { BrowserTerminal } from '@benjamin-small/browser-terminal';
import { installFilesystem, type BrowserFilesystem } from '@benjamin-small/browser-terminal/filesystem';
import { createTextEditor } from '@benjamin-small/browser-terminal/filesystem/editor';

declare global { interface Window { filesystem: BrowserFilesystem } }

/** Browser-private files are ready at startup; local folders are optional. */
export async function filesystemDemo(bt: BrowserTerminal): Promise<void> {
  const editor = createTextEditor();
  const filesystem = installFilesystem(bt, { editor: editor.open, devices: true, initialDirectory: '/scratch', onDirectoryChange: () => bt.refreshPrompt() });
  bt.setPrompt(({ session }) => `${filesystem.pwd(session)} `);
  window.filesystem = filesystem;
  // This demo owns its redirect policy. Installing the library adapter alone
  // continues to leave a host application's existing redirect handler untouched.
  bt.setRedirectHandler(filesystem.createRedirectHandler());
  const section = document.createElement('section');
  section.setAttribute('aria-label', 'Browser filesystem');
  const heading = document.createElement('h2'); heading.textContent = 'Your browser files';
  const description = document.createElement('p');
  description.textContent = 'Start with ls or edit welcome.txt. Tab completes commands, paths, and flags. These files stay in this browser for this site; clearing site data removes them. Connect a local folder only when you want to work on files outside the browser.';
  const status = document.createElement('p'); status.setAttribute('role', 'status');
  const example = document.createElement('pre'); example.textContent = 'ls\nedit welcome.txt\necho hello > hello.txt\ncat hello.txt';
  const controls = document.createElement('div'); controls.style.cssText = 'display:flex;gap:8px;flex-wrap:wrap';
  section.append(heading, description, controls, status, example);
  document.querySelector('main')!.append(section);
  const report = (error: unknown) => { status.textContent = error instanceof Error ? error.message : String(error); };
  const button = (label: string) => { const el = document.createElement('button'); el.textContent = label; el.style.cssText = 'padding:8px;font:inherit'; controls.append(el); return el; };
  const home = button('Browser files'); home.disabled = true;
  const connect = button('Connect local folder');
  const picker = (window as unknown as { showDirectoryPicker?: (options: { mode: 'read' }) => Promise<FileSystemDirectoryHandle> }).showDirectoryPicker;
  connect.disabled = !picker || !window.isSecureContext;
  let disposed = false;
  connect.addEventListener('click', async () => {
    const session = bt.snapshot!.sessions.find(item => item.active)!.id;
    connect.disabled = true;
    status.textContent = 'Choose a local folder in the browser’s folder picker…';
    let mountedPath: string | undefined;
    try {
      // Request only browsing access here. A missing write grant must not block
      // mounting; the editor requests it separately from its Enable writes action.
      // Keep the picker directly in this click's user activation, before any await.
      const handle = await picker!.call(window, { mode: 'read' });
      if (disposed) return;
      mountedPath = filesystem.mountLocal(handle, { writable: true });
      await filesystem.cd(mountedPath, { session });
      status.textContent = `Connected ${handle.name} at ${mountedPath}. Browser files remain at /scratch. Enable writes in the editor when needed. Local folders must be reconnected after a page reload.`;
    } catch (error) {
      if (mountedPath && !disposed) filesystem.unmount(mountedPath);
      if (disposed) return;
      if (error instanceof DOMException && error.name === 'AbortError') {
        status.textContent = `No folder connected. The browser did not return a folder handle. This can happen after selection if access is denied, if the folder is restricted, or if the picker is cancelled. If you selected a folder in an embedded browser but no permission prompt appeared, open this page in external Chrome or Edge. Browser files remain available at /scratch. Browser detail: ${error.name}: ${error.message}.`;
      } else {
        status.textContent = `Could not connect a local folder: ${error instanceof Error ? `${error.name}: ${error.message}` : String(error)}. Browser files remain available.`;
      }
    } finally {
      connect.disabled = disposed || !picker || !window.isSecureContext;
    }
  });
  home.addEventListener('click', () => {
    const session = bt.snapshot!.sessions.find(item => item.active)!.id;
    void filesystem.cd('/scratch', { session }).then(() => { status.textContent = 'Browser files ready at /scratch.'; }).catch(report);
  });
  status.textContent = 'Opening browser files…';
  try {
    if (!navigator.storage?.getDirectory) throw new Error('Browser-private storage is unavailable in this context');
    const root = await navigator.storage.getDirectory();
    // Retain the existing demo directory so earlier scratch files survive this change.
    const dir = await root.getDirectoryHandle('browser-terminal-demo', { create: true });
    try { await dir.getFileHandle('welcome.txt'); }
    catch (error) {
      if (!(error instanceof DOMException) || error.name !== 'NotFoundError') throw error;
      const handle = await dir.getFileHandle('welcome.txt', { create: true });
      const writer = await handle.createWritable();
      try {
        await writer.write('Welcome to your browser filesystem.\nEdit this file and choose Save. No folder permissions are needed.\n');
        await writer.close();
      } catch (error) { await writer.abort().catch(() => {}); throw error; }
    }
    filesystem.mount('scratch', dir, { writable: true });
    home.disabled = false;
    status.textContent = connect.disabled
      ? 'Browser files ready at /scratch. Connecting local folders is unavailable in this browser.'
      : 'Browser files ready at /scratch.';
  } catch (error) {
    status.textContent = `Could not open browser storage: ${error instanceof Error ? error.message : String(error)}. The terminal is still available.`;
  }
  bt.onLifecycle(event => {
    if (event.type === 'dispose') {
      disposed = true;
      connect.disabled = home.disabled = true;
      // Editors remain available for exporting dirty buffers after disconnection.
    }
  });
}
