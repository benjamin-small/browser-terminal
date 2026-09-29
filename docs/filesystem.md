# Browser files and editing

The optional filesystem adapter connects browser directory handles to shell
commands. It keeps filesystem access in the host; importing the terminal alone
adds no mounts, commands, permission prompts, or editor code.

The demo starts automatically in writable browser-private storage at `/scratch`.
`ls`, `edit welcome.txt`, and `echo hello > hello.txt` work without a folder
picker or permission request. Browser files survive reloads. This is OPFS,
not the HTTP response cache: it belongs to this website's origin and browser
profile, is subject to quota/eviction, and is deleted when site data is cleared.
Connecting a local folder is optional and requires browser consent.

To give your own application the same startup behavior:

```ts
const filesystem = installFilesystem(terminal, {
  editor: editor.open,
  initialDirectory: '/scratch',
  onDirectoryChange: () => terminal.refreshPrompt(),
});
terminal.setPrompt(({ session }) => `${filesystem.pwd(session)} `);
await filesystem.mountScratch('scratch', { writable: true });
terminal.setRedirectHandler(filesystem.createRedirectHandler());
```

The demo uses a `browser-terminal-demo` subdirectory of OPFS to preserve its
existing files. If browser storage is unavailable, it reports the failure and
keeps the terminal usable at `/`, without opening a local-folder picker.

## Connect a directory

```ts
import { BrowserTerminal } from '@benjamin-small/browser-terminal';
import { installFilesystem } from '@benjamin-small/browser-terminal/filesystem';
import { createTextEditor } from '@benjamin-small/browser-terminal/filesystem/editor';

const terminal = await BrowserTerminal.create();
const editor = createTextEditor();
const filesystem = installFilesystem(terminal, { editor: editor.open });

connectButton.addEventListener('click', async () => {
  // Feature-detect showDirectoryPicker in the host before enabling this button.
  // Some TypeScript DOM libraries need an ambient declaration for the picker.
  const handle = await window.showDirectoryPicker({ mode: 'read' });
  const path = filesystem.mountLocal(handle, { writable: true });
  // For a folder named projects, path is /mnt/projects.
  // Use path with cd(), unmount(), and requestWritePermission().
});
```

`writable: true` permits the adapter to write, but does not grant browser write
permission. The demo requests `read` when connecting so missing write permission
does not prevent browsing. When editing needs write access, call
`filesystem.requestWritePermission('/mnt/projects')`
directly from an Enable writes button. The supplied editor shows this action
only when write permission is missing. OPFS needs no permission grant. Omitting
`writable` makes the mount read-only regardless of browser permissions.

The picker requires a secure context and user activation, and is not supported
in every browser. Feature-detect `window.showDirectoryPicker` and each required
storage method. Programmatic shell commands do not open permission dialogs.
See [the directory picker API](https://developer.mozilla.org/en-US/docs/Web/API/Window/showDirectoryPicker).

A host can use its own editor with `editor: document => openMyEditor(document)`.
The callback receives bounded `read()`, conflict-checked `save(bytes, original)`,
`writePermission()` (inspection without prompting), `requestWritePermission()`,
and a disconnection signal. It should return when
opened, not when the user eventually closes the editor.

For private scratch storage:

```ts
await filesystem.mountScratch('scratch', { writable: true });
```

OPFS is private to the website and subject to browser quota and eviction. It
is not the user's selected local folder. OPFS contents persist across reloads;
the host remounts them at startup. Selected local-folder handles are not remembered
across page loads in this version. See [OPFS](https://developer.mozilla.org/en-US/docs/Web/API/File_System_API/Origin_private_file_system).

`mountLocal(handle)` maps a selected folder at `/mnt/<folder-name>` and returns
that path. Duplicate names get `-2`, `-3`, etc.; `{ name: 'work' }` chooses
`/mnt/work`. `/mnt` always exists, even when empty. The demo connects local
folders this way and changes to their mount path. Browser files stay at `/scratch`.
For explicit host layouts, `mount('custom/path', handle)` also supports nested
mounts; overlapping mount paths are rejected to avoid hiding other mounts.
The examples below assume a selected folder named `projects`.

When connecting a local folder, the demo shows a pending selection message and
disables duplicate connection attempts. Cancellation or a browser error leaves
existing mounts unchanged and shows an explanation. If no chooser appears,
try the same page in Chrome or Edge. A successful connection reports the exact
`/mnt/...` path. Local selections must be reconnected after a page reload.
`AbortError` does not establish that the user cancelled: browsers also use it
when the selected folder is restricted or the requested access was not granted.
The demo includes the original error name and message in its status.

An embedded browser can expose `showDirectoryPicker` and show a folder chooser
without completing the native permission approval. In our embedded-browser
check, selecting a folder returned `AbortError` without a permission prompt;
the same flow worked in external Chrome, as confirmed by the user. API presence
alone therefore does not guarantee a working mount. If you see this behavior,
open the page in external Chrome or Edge for local folder access. Browser-private
files at `/scratch` remain available independently of the local folder picker.

## Commands

The demo prompt shows the current directory, for example `/scratch ❯`, and
follows `cd`, session switches, and split panes. `cat welcome.txt` prints a
file's contents. Press Tab at the end of a command prefix to complete names;
this also works after `|` and `;` and for multiword names such as `str upcase`.
Ambiguous prefixes expand to the shared prefix; another Tab lists candidates.
The adapter also completes paths for `cat`, `cd`, `ls`, `edit`, `read-bytes`,
and redirect targets. `cat <Tab>` lists entries; `cat wel<Tab>` completes a
matching filename. `cd` suggests only directories. Relative paths, absolute
mount paths, and quoted names work; directory suggestions end in `/` so another
Tab can list their children. Names with spaces or shell punctuation are quoted
automatically. Listings scan at most 5,000 entries and return at most 200 matches.
Completion never requests permissions and discards results after input or
directory changes. Flags such as `ls --lo<Tab>` complete from command signatures.

```sh
ls /
cd /mnt/projects/src; ls
pwd
ls --long | filter {|entry| $entry.kind == 'file'}
cat 'notes with spaces.txt'
read-bytes image.png --offset 0 --length 16
edit notes.txt
```

`/` lists top-level directories; `ls /mnt` lists connected local folders. `ls` emits records with `name`, `kind`, and `path`; `--long`
adds `size` and `modified` (milliseconds since the Unix epoch, or null for
folders). Listings stream without loading the whole tree. A single result is
still a collection. Metadata reads are sequential to bound resource use.

Paths use `/`, `.`, and `..`. Going up from `/mnt/projects` returns virtual `/mnt`, then `/`,
never the selected folder's real parent. Absolute paths are virtual, not OS
paths. Quote names containing shell operators or spaces. There is no implicit
URL decoding, tilde expansion, globbing, or case folding.

Cwd belongs to a session. Split panes share it; new sessions start at `/` unless
the host sets `initialDirectory`. That default applies until a session explicitly
changes directory and falls back to `/` while its mount is unavailable.
Failed `cd` leaves cwd intact; `cd` without an argument goes to `/`. Commands
capture paths at operation start. Pipeline stages run concurrently, so use
`cd src; ls` for sequencing rather than `cd src | ls`. Redirects resolve paths
when their hooks run; use absolute destinations if another pane can change cwd.

`filesystem.unmount('/mnt/projects')` cancels outstanding access, resets affected cwd
values to `/`, and leaves editor text available for export. Closing a session
or disposing the terminal also invalidates its resources. Calling
`filesystem.dispose()` removes only commands still owned by that adapter; it
never removes a host's replacement command. Only one adapter may be installed
on a terminal. Existing command-name collisions cause installation to fail
and roll back; existing host commands are not replaced.

## Saving and redirection

Install redirects explicitly. This calls the terminal's existing setter and
therefore replaces any handler previously installed by the host:

```ts
terminal.setRedirectHandler(filesystem.createRedirectHandler());
```

```sh
echo hello > /scratch/hello.txt
echo world >> /scratch/hello.txt
ls /scratch | to json > /scratch/list.json
str upcase < /scratch/hello.txt
```

Reads decode UTF-8 with invalid sequences rejected. Pass `{ binary: true }` to
the handler factory to read byte buffers instead. Writes accept strings
(encoded as UTF-8) or `Uint8Array`; serialize other values explicitly. No
newline is inserted. `>` replaces, `>>` appends, and either can create a file
in an existing parent directory. A failed producer does not call the writer.

Writes are staged until the writable stream closes successfully. Errors and
cancellation abort staged content; a completed close cannot be rolled back.
Creating a new file may leave an empty directory entry if a subsequent write
fails. Browser writes are not an OS durability guarantee. See
[write semantics](https://developer.mozilla.org/en-US/docs/Web/API/FileSystemWritableFileStream/write).

The adapter serializes all its writes, including aliases through overlapping
mounts. A manually opened transaction holds this queue until committed or
aborted. This does not lock out other tabs or native applications.

## Text editor

The optional editor provides Save, Reload, Enable writes, Export text, and Close.
Ctrl/Cmd-S saves. Escape closes with a discard decision when necessary. Errors
retain the buffer; disconnecting a mount disables saving and leaves export
available. An unchanged save does not rewrite the file.

Only existing valid UTF-8 files are edited. UTF-8 BOM and LF/CRLF style are
preserved; mixed endings or lone carriage returns open read-only. Before a
save, the adapter compares the current file bytes with the original buffer.
If they differ, reload or export the unsaved work. There remains a race between
checking and closing the write stream: browser APIs provide no general atomic
compare-and-swap against external applications.

`editor.dispose()` explicitly closes all editor windows and discards their
buffers. Normal terminal disposal leaves disconnected editors available for
export. Call editor disposal only when the host intends to discard them.

## Byte I/O and experimental devices

```ts
const context = { session: terminal.snapshot!.sessions.find(s => s.active)!.id };
const reader = await filesystem.openReader('/mnt/projects/image.bin', context);
const header = await reader.readAt(0, 16);
for await (const chunk of reader.chunks()) {
  // Process bounded byte chunks, with backpressure from your consumer.
}
const tx = await filesystem.beginWrite('/mnt/projects/image.bin', context);
try {
  await tx.writeAt(8, new Uint8Array([1, 2, 3]));
  await tx.commit();
} catch (error) {
  await tx.abort();
  throw error;
}
```

Readers are snapshots. Reopen after a write to see new contents. Offsets and
lengths must be nonnegative safe integers; reads beyond EOF return empty bytes,
and ranges crossing EOF return remaining bytes. Transactions also offer
`truncate(size)`. `size` records the file size at transaction open.

Enable virtual devices separately with `{ devices: true }` when installing.
`read-bytes /dev/zero --length 16` returns bounded zero bytes; `/dev/null` reads
EOF and discards direct byte writes. `/dev/zero` rejects writes. Device reads
require bounds, and generic `cat` and redirect hooks reject devices to avoid
unbounded collection.

The experimental `openBlockDevice(filesystem, path, context, blockSize = 512)`
export wraps a regular file. Its `read(block, count = 1)` and
`write(block, bytes)` operate on aligned blocks within a fixed capacity. Each
write commits independently. File size must be a multiple of the block size;
a subsequent size change is rejected. Inspect `capabilities` before use. This
API manipulates file bytes; it does not mount disk-image formats or expose OS
devices. Native devices and general live character streams are not provided.

## Limits and verification

Defaults are 8 MiB per buffered read or write chunk, 2 MiB per editor file, and
64 KiB per streamed chunk. Configure `maxReadBytes`, `maxEditorBytes`, and
`chunkSize` at installation. Existing shell redirection collects the pipeline
before invoking the adapter: its limit cannot bound upstream collection memory.
Use the byte API for large transfers. Ctrl-C is checked around asynchronous
operations; already-running browser reads cannot always be interrupted.

Automated coverage includes a deterministic memory filesystem and real OPFS
through the WASM shell in Playwright Chromium. Native folder-picker permissions
and saves require the manual checklist in `packages/demo/TESTING.md`. A passed
OPFS test does not establish native-folder support in another browser or in a
standalone `file://` page.
