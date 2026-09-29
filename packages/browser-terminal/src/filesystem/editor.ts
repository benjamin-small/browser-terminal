import type { EditorDocument } from './types.js';
import { sameBytes } from './paths.js';

/** A lossless UTF-8 text model for the optional editor. */
export function textModel(bytes: Uint8Array) {
  const bom = bytes.length >= 3 && bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf;
  const decoded = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  const crlf = decoded.includes('\r\n');
  const remainder = decoded.replace(/\r\n/g, '');
  const mixed = remainder.includes('\r') || (crlf && remainder.includes('\n'));
  return {
    text: decoded.replace(/\r\n/g, '\n'),
    mixed,
    encode(text: string): Uint8Array {
      const content = crlf ? text.replace(/\r?\n/g, '\r\n') : text;
      const encoded = new TextEncoder().encode(content);
      if (!bom) return encoded;
      const result = new Uint8Array(encoded.length + 3);
      result.set([0xef, 0xbb, 0xbf]); result.set(encoded, 3);
      return result;
    },
  };
}

/** Optional editor UI. No dependency on xterm, WASM, or the default package entrypoint. */
export function createTextEditor(options: { mount?: HTMLElement; confirmDiscard?: () => boolean } = {}) {
  const closeAll = new Set<() => void>();
  const open = async (document: EditorDocument): Promise<void> => {
    let original = await document.read();
    let model = textModel(original);
    document.signal.throwIfAborted();
    const previous = window.document.activeElement;
    const host = window.document.createElement('section');
    const shadow = host.attachShadow({ mode: 'open' });
    const style = window.document.createElement('style');
    style.textContent = `
      :host{position:fixed;inset:8vh 8vw;z-index:2147483647;font:14px system-ui;color:#e4e4e7}
      section{height:100%;display:flex;flex-direction:column;background:#18181b;border:1px solid #71717a;border-radius:8px;padding:16px;box-sizing:border-box;box-shadow:0 12px 60px #0008}
      h2{font-size:16px;margin:0 0 8px;overflow-wrap:anywhere} textarea{flex:1;min-height:100px;resize:none;background:#09090b;color:inherit;font:14px/1.5 monospace;padding:12px;border:1px solid #71717a}
      footer{display:flex;gap:8px;flex-wrap:wrap;padding-top:10px}button{font:inherit;padding:6px 12px;cursor:pointer}p{min-height:20px;margin:8px 0;white-space:pre-wrap}button:focus-visible,textarea:focus-visible{outline:2px solid #60a5fa;outline-offset:2px}
    `;
    shadow.append(style);
    const panel = window.document.createElement('section');
    panel.setAttribute('role', 'dialog'); panel.setAttribute('aria-label', `Edit ${document.path}`);
    const title = window.document.createElement('h2'); title.textContent = document.path;
    const status = window.document.createElement('p'); status.setAttribute('role', 'status');
    const textarea = window.document.createElement('textarea');
    textarea.setAttribute('aria-label', `Contents of ${document.path}`);
    textarea.spellcheck = false; textarea.value = model.text;
    const footer = window.document.createElement('footer');
    panel.append(title, textarea, status, footer); shadow.append(panel);
    let busy = false;
    let closed = false;
    const dirty = () => !model.mixed && !sameBytes(model.encode(textarea.value), original);
    const discard = () => !dirty() || (options.confirmDiscard?.() ?? window.confirm('Discard unsaved changes?'));
    const report = (error: unknown) => { status.textContent = error instanceof Error ? error.message : String(error); };
    const button = (label: string, action: () => void | Promise<void>) => {
      const el = window.document.createElement('button'); el.textContent = label;
      el.addEventListener('click', () => { if (!busy) void Promise.resolve().then(action).catch(report); });
      footer.append(el); return el;
    };
    const refresh = () => {
      textarea.readOnly = !document.writable || model.mixed || document.signal.aborted;
      save.disabled = textarea.readOnly || busy;
      reload.disabled = busy || document.signal.aborted;
      permission.disabled = busy || !document.writable || document.signal.aborted;
      close.disabled = busy;
      title.textContent = `${document.path}${dirty() ? ' *' : ''}`;
      status.textContent = document.signal.aborted ? 'Disconnected. Export your text before closing.'
        : model.mixed ? 'Read-only: mixed or unsupported line endings.'
        : !document.writable ? 'Read-only mount.' : dirty() ? 'Unsaved changes' : 'Saved';
    };
    const saveNow = async () => {
      if (busy || textarea.readOnly) return;
      const bytes = model.encode(textarea.value);
      if (sameBytes(bytes, original)) return;
      busy = true; refresh();
      try { await document.save(bytes, original); original = bytes; }
      finally { busy = false; refresh(); }
    };
    const save = button('Save', saveNow);
    const reload = button('Reload', async () => {
      if (!discard()) return;
      busy = true; refresh();
      try { const bytes = await document.read(); const next = textModel(bytes); original = bytes; model = next; textarea.value = model.text; }
      finally { busy = false; refresh(); }
    });
    // Direct click listener preserves browser user activation for permission prompts.
    const permission = window.document.createElement('button'); permission.textContent = 'Enable writes';
    permission.addEventListener('click', () => {
      if (busy) return;
      void document.requestWritePermission().then(granted => {
        status.textContent = granted ? 'Write permission granted. Choose Save to apply changes.' : 'Write permission denied.';
      }).catch(report);
    });
    footer.append(permission);
    button('Export text', () => {
      const bytes = model.mixed ? original : model.encode(textarea.value);
      const url = URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: 'text/plain;charset=utf-8' }));
      const link = window.document.createElement('a'); link.href = url; link.download = document.path.split('/').at(-1) ?? 'unsaved.txt';
      link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
    });
    const finish = () => {
      if (closed) return;
      closed = true; host.remove(); closeAll.delete(finish);
      document.signal.removeEventListener('abort', refresh);
      window.removeEventListener('beforeunload', beforeUnload);
      if (previous instanceof HTMLElement && previous.isConnected) previous.focus();
    };
    const close = button('Close', () => { if (discard()) finish(); });
    const beforeUnload = (event: BeforeUnloadEvent) => { if (dirty()) { event.preventDefault(); event.returnValue = ''; } };
    window.addEventListener('beforeunload', beforeUnload);
    document.signal.addEventListener('abort', refresh, { once: true });
    textarea.addEventListener('input', refresh);
    panel.addEventListener('keydown', event => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') { event.preventDefault(); void saveNow().catch(report); }
      if (event.key === 'Escape' && !busy) { event.preventDefault(); if (discard()) finish(); }
    });
    closeAll.add(finish); (options.mount ?? window.document.body).append(host); refresh(); textarea.focus();
  };
  return {
    open,
    /** Explicit host teardown discards buffers; normal Close offers a discard decision. */
    dispose() { for (const close of [...closeAll]) close(); },
  };
}
