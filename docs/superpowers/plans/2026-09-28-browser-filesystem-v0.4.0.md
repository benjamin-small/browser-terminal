# Browser filesystem and editing: v0.4.0 implementation plan

Status: implemented release candidate, with final release gates pending.
Target and repository version: v0.4.0. Planning date: 2026-09-28.
Tracking: [issue #25](https://github.com/benjamin-small/browser-terminal/issues/25).

Implementation decisions: the small mount/session/command/redirect modules are
consolidated in `filesystem/index.ts`; byte transactions, paths, types, devices,
and editor remain separate. All adapter writes share a conservative queue,
which covers overlapping mount aliases without assuming path identity.

Local validation passes: 274 native tests, 48 WASM boundary tests, 18 filesystem
contract tests, 36 Chromium browser tests, and six filesystem tests each in
Firefox 151 and WebKit 26.5. WebKit tests use disposable persistent profiles
because OPFS failed in its ephemeral context. Rust formatting and Clippy pass;
the baseline formatting cleanup is a separate commit. All three built demos
passed artifact verification. npm reports 0.3.0 as current and no 0.4.0 release.

Manual Chrome 154 checks verified the real folder picker, local navigation,
reading, opening a local file, and refusal to save without write permission.
Automated approval review rejected granting write access to the generated
`/tmp/browser-terminal-native-fixture`; native save verification remains pending
explicit user authorization. No release tag or npm publication has occurred.
The remaining checklist is in the draft release notes.

## Outcome and release scope

A user connects a local folder, navigates it with `pwd`, `cd`, and `ls`, reads
files, and opens a text file in an editor alongside the terminal. Saving writes
back to the connected folder. An optional browser-private scratch directory
uses the same commands. File-backed block operations and a small virtual device
demonstration exercise the same byte I/O layer.

The adapter is opt-in. Existing command registration, structured pipes,
redirection, and applications without a filesystem retain their behavior.
The Rust core continues to treat resources as host-owned objects, as specified
in [language direction](../../language-direction.md).

Release requirements:

- Selected local directory mounts and optional OPFS scratch storage.
- `pwd`, `cd`, structured `ls`, `cat`, `read-bytes`, and `edit`.
- Explicitly enabled UTF-8/byte file writes and filesystem redirection.
- A small accessible text editor with Save, Reload, and dirty-buffer handling.
- Capability-based byte I/O, including bounded offset reads and staged writes.
- An experimental file-backed block adapter and virtual `/dev/null` and
  `/dev/zero`, enabled separately from ordinary filesystem commands.
- Consumer documentation, browser tests, and packed-package verification.

Follow-ups after v0.4.0: path completion, per-session directory prompts,
remembered mounts, syntax highlighting, a terminal-mode editor, full hex editing,
recursive file operations, rename/move, filesystem watchers, and general live
character streams. No raw disk, OS device, POSIX descriptor, or native process
access is implied by this release.

## Findings from the current implementation

- `packages/browser-terminal/src/index.ts` exposes async command registration
  and an optional redirect handler. A second handler replaces the first.
- `src/types.ts` exposes session/pane IDs and abort signals to commands and
  redirects. Values already include `Uint8Array`.
- The package README already demonstrates a session-keyed directory map.
  Split panes share a session; a new session needs its own initial directory.
- `crates/bterm-core/src/eval.rs` runs pipeline stages concurrently and invokes
  an output redirect only after successful collection of the whole output.
  Existing redirects are not a streaming byte transport.
- `setPrompt()` changes every pane. The line editor has no implemented host
  completion API. Neither should block basic filesystem support.
- Session-close events exist internally, but the public API lacks adapter
  lifecycle subscriptions. Registration also replaces existing host commands
  without ownership tracking. Both require focused integration work.
- CI runs native tests, WASM boundary tests, and type checking. The release
  workflow additionally packs and boots a consumer, but does not currently run
  the complete Playwright suite or all AGENTS.md Rust checks.

## Browser constraints and the I/O contract

The folder picker needs a secure context and transient user activation. Keep
selection and permission prompts in a host button's user-gesture handler;
commands with missing permission return an actionable error. Feature-detect
each capability. A programmatic `run()` must not assume permission to prompt.
See [showDirectoryPicker](https://developer.mozilla.org/en-US/docs/Web/API/Window/showDirectoryPicker).

A directory handle grants access to the selected tree. Our `/local` path is a
virtual mount name; it is not the machine's absolute path. OPFS provides a
separate origin-private tree, subject to storage quota and eviction; it is not
a substitute for connecting the user's existing folder. See
[OPFS](https://developer.mozilla.org/en-US/docs/Web/API/File_System_API/Origin_private_file_system).

For ordinary browser files, offset writes, seeking, and truncation are staged
in a writable stream. The underlying file is updated on successful close.
Expose an explicit commit/abort lifecycle and do not promise immediate disk
writes or OS-level durability. See
[writable file streams](https://developer.mozilla.org/en-US/docs/Web/API/FileSystemWritableFileStream/write).

OPFS synchronous access is restricted to dedicated workers and OPFS files.
Do not use it as the default local-folder implementation or require a worker
for this release. See
[synchronous access handles](https://developer.mozilla.org/en-US/docs/Web/API/FileSystemSyncAccessHandle).

## Architecture and decisions

Add optional package exports `@benjamin-small/browser-terminal/filesystem` and
`@benjamin-small/browser-terminal/filesystem/editor`. Keep editor code and styles
out of the default import. Proposed files live under
`packages/browser-terminal/src/filesystem/`; names below are implementation
targets, not existing APIs.

| Module | Responsibility |
| --- | --- |
| `types.ts` | Resource capabilities, entries, errors, byte readers, write transactions |
| `paths.ts` | Pure normalization and mount-relative resolution |
| `mounts.ts` | Mount registry, access policy, identity, unmount generation |
| `directory.ts` | Local directory-handle and OPFS operations |
| `session.ts` | Current directory, operation snapshots, cleanup |
| `commands.ts` | Navigation, text/byte reads, edit dispatch |
| `redirect.ts` | Explicit filesystem redirect-handler factory |
| `io.ts` | Bounded reads, write transactions, same-file write serialization |
| `devices.ts` | Experimental null/zero and file-backed block adapters |
| `editor.ts` | Optional DOM editor and host editor integration |
| `index.ts` | Public opt-in installer and types |

### Paths, mounts, and sessions

- `/` is a synthetic directory listing mounted roots. Use `/local` for the
  first connected directory and `/scratch` for explicitly enabled OPFS.
  Allow additional unique mount names; reject nested or conflicting mounts.
- Resolve `/`, repeated separators, `.`, `..`, and quoted Unicode names.
  Treat separators as `/`, with no URL decoding, tilde expansion, or globbing.
  `..` at a mount root returns virtual `/`; at `/` it stays there. It never
  requests the selected folder's actual parent. Preserve filename casing.
- Cwd is session-scoped; new sessions start at `/`. `cd` without a path goes
  to `/`. The connect UI may explicitly change the initiating session's cwd.
- Validate directories before committing `cd`. Serialize cwd mutations within
  a session. A failed or cancelled change leaves the prior directory intact.
- Capture cwd, mount identity, and access policy when each operation starts,
  before its first await. Pending reads never switch destination after a tab
  switch, cwd change, unmount, or remount. Invalidate pending writes on unmount.
- Pipelines are concurrent: `cd src | ls` does not promise sequencing. Document
  `cd src; ls`. Redirect paths resolve when their hooks start; for a concurrent
  cwd-changing operation, use absolute paths when the destination must be fixed.
  Pipeline-wide cwd snapshots would require a separate core contract.
- Unmount resets affected session directories to `/`, invalidates operations,
  and retains dirty editor text for recovery. Terminal disposal removes adapter
  subscriptions and aborts outstanding operations.

### Data, permissions, and writes

- `ls` yields `{ name, kind, path }` records; `ls --long` adds metadata using
  bounded concurrency. Avoid eager full-tree scans or mandatory file reads.
- `cat` returns bounded UTF-8 text with fatal decode errors; `read-bytes`
  returns bytes with optional offset and length. Use documented, configurable
  limits: initially 8 MiB per buffered read/redirect payload, 2 MiB per editor
  file, and 64 KiB per streamed byte chunk. Reject oversize buffered operations
  before reading when size is known. Measure and revise before API freeze.
- Mounts default to read-only. Write access requires both host policy and
  browser permission. OPFS writes likewise require host opt-in.
- Expose a redirect-handler factory; installation never silently takes over
  an existing handler. The host explicitly installs or composes that handler.
  Default reads decode UTF-8; offer a byte-read handler option. Writes accept
  only strings or `Uint8Array`. Records/lists require `to json` or another
  explicit serializer. Do not insert an implicit newline.
- `>` replaces contents, `>>` appends; a missing file may be created only in an
  existing writable parent. Test empty and binary payloads. Existing redirects
  collect output before the adapter sees it: the adapter's payload limit cannot
  bound upstream collection memory. Large transfers use the byte API.
- A byte reader exposes bounded `readAt(offset, length)` and chunked reading.
  A write transaction exposes `writeAt`, `truncate`, `commit`, and `abort`.
  A cursor wrapper may implement `read`/`write`/`seek` over those primitives.
  Validate nonnegative safe-integer offsets and lengths. Read-at-EOF returns
  empty bytes; reads overlapping EOF return the remaining bytes.
- Read handles are snapshots; reopen after commit. Do not imply read-your-writes
  through an older reader. Partial updates preserve untouched bytes using
  `createWritable({ keepExistingData: true })`; replacement can truncate.
- Serialize writes to the same underlying file within the adapter, including
  overlapping mounts (handle identity, not path strings alone). Recheck access
  and cancellation immediately before commit. Abort staged writes on errors.
  Cancellation after a successful close cannot roll back a completed write.
- External changes are detected on a best-effort basis. Compare fresh size and
  modification metadata and, for bounded editor files, original bytes before
  save. Browser APIs provide no general compare-and-swap against external apps;
  a race remains between checking and closing. Document that limitation.

### Editing and virtual devices

- `edit path` dispatches to a host-supplied editor callback. Supply an optional
  lightweight textarea editor for the demo and consumers without an editor.
  Opening it returns promptly; the edit buffer has its own lifetime.
- Include a filename, read-only/dirty state, labelled Save/Reload/Close controls,
  keyboard save, and focus restoration. Save failures retain the buffer.
  Closing or reloading a dirty buffer requires a discard decision. The UI can
  enable write permission through a direct user action.
- Edit existing valid UTF-8 files initially. Preserve BOM and line-ending style;
  treat mixed line endings as read-only until a lossless policy is implemented.
  Avoid silently changing bytes in an unchanged file. Detect stale contents
  before save; offer reload or export of the unsaved buffer on conflict.
- A block adapter wraps a regular file with fixed capacity and a configurable
  block size (default 512 bytes). Require aligned operations and a file size
  divisible by the block size. Reject out-of-range access; updates commit as a
  transaction. This is a disk-image-like interface, not a mounted disk format.
- Virtual `/dev/null` returns EOF and discards writes. `/dev/zero` provides
  bounded zero-filled chunks and rejects writes. Byte APIs must require bounds
  or cancellation; `cat /dev/zero` and generic buffered redirects to/from live
  devices are rejected. Do not route infinite streams into the collecting
  redirect API. General host-connected character streams are a follow-up.

## Work packages and acceptance gates

Complete in dependency order. Each package should become a focused issue/PR.
CONTRIBUTING.md requests an issue before substantial changes; use this document
as the issue body/source when implementation starts.

1. **Adapter contract and lifecycle.** Add public types, pure path resolution,
   capability checks, and optional exports. Add the minimal lifecycle and
   registration ownership support needed for clean installation/disposal;
   reject command collisions rather than replacing host commands. Tests cover
   path edge cases, invalid offsets, mount identity, repeated installation,
   cleanup, and preservation of unrelated commands and redirect handlers.
2. **Mounts and navigation.** Implement the directory provider, OPFS provider,
   session cwd, and `pwd`/`cd`/`ls`. Add a Connect folder demo button. Test nested
   and Unicode names, root confinement, denied/revoked access, missing entries,
   session sharing/isolation, async session switches, unmount/remount, and
   cancellation. A real selected folder must work in a manual browser test.
3. **Reads and transactional writes.** Implement byte/text reads, the write
   transaction, and opt-in redirects. Test byte fidelity, invalid UTF-8, limits,
   overwrite/append/truncate, offset patches preserving surrounding bytes,
   same-file writes through overlapping mounts, failed pipelines, aborted
   writes, close failures, and quota errors. No success before commit resolves.
4. **File editor.** Implement the host callback and optional DOM editor.
   Browser tests cover edit/save/reopen, keyboard/focus behavior, BOM/line
   endings, dirty close, stale-file conflicts, denied permissions, unmount,
   and failure recovery. Saving must use the same write layer as redirects.
5. **Experimental devices.** Implement file-backed blocks and null/zero.
   Test alignment, capacity, EOF, byte accuracy, commit/abort, bounded memory,
   cancellation, and rejection of unbounded use. Keep this API explicitly
   experimental and independently enabled.
6. **Documentation and release candidate.** Add a filesystem guide and examples,
   update the manual checklist and feature matrix, add optional entrypoints to
   package guards/consumer tests, and enforce the release gates below. Move
   draft notes to confirmed release notes only for completed functionality.

## Validation and release readiness

Baseline checked while preparing this plan (2026-09-28):

- `cargo clippy --all-targets --all-features`: passed.
- `cargo test --all-features`: passed for the native target. This does not run
  the WASM boundary suite.
- `cargo fmt --all --check`: failed on pre-existing formatting differences
  across the Rust sources, including `crates/bterm-cli/src/main.rs` and the
  core/WASM crates. Resolve in a separate formatting-only change before the
  release gate is enforced; this documentation change leaves those files alone.
- Planning-document local links and whitespace checks: passed. Browser, WASM,
  package, and release-workflow checks were not run for this documentation-only
  change; they remain implementation/release requirements.

Use a deterministic in-memory provider for failure injection and pure contract
tests, plus actual OPFS in browser tests. Mocking the native picker is useful
for CI but cannot prove local permission behavior; retain manual picker/save
checks against a disposable fixture directory. Record tested browser versions.

Test the local-folder flow in current Chrome and Edge. Feature-detect and test
the unavailable-picker experience in Firefox and Safari, and exercise OPFS
where its required methods exist. Unsupported capabilities must leave the
terminal usable and explain what is available. Do not promise native-folder
support for the standalone `file://` demo without verifying it.

Required checks for the implementation:

```sh
npm ci
cargo fmt --all --check
cargo clippy --all-targets --all-features
cargo test --all-features
just test-wasm
just typecheck
just test-e2e
```

Add filesystem unit tests to a documented command and wire it into CI and the
release workflow. Add fmt/clippy/all-features and the complete browser suite to
the release gate; existing release checks alone are insufficient. Build all
three demos and verify the assembled site using the Pages workflow recipe.

Before publishing:

- Resolve experimental API names and confirm every release requirement above.
- Check that v0.4.0 is available on npm; repository version alone does not prove
  the next registry version is free.
- Update `Cargo.toml` workspace version, the three workspace package entries in
  `Cargo.lock`, `packages/browser-terminal/package.json`, and the workspace
  package entry in `package-lock.json` using the package managers. Leave unrelated
  dependency versions and private demo versions alone.
- From a clean committed checkout, rebuild, run all gates, run the publishable
  check before packing, pack, and run `scripts/verify-tarball.mjs` on that exact
  artifact. Extend it to import and exercise both new optional entrypoints.
- Run the release workflow with `dry_run: true`. Its registry check requires
  the candidate version to be unpublished even for a dry run.
- Finalize notes with supported browsers, permission flow, limits, experimental
  API status, and opt-in upgrade examples. Existing consumers need no migration
  unless they choose the new adapter.
- Publish through the existing `v0.4.0` tag workflow only when the release is
  ready. Verify the registry artifact and published demos afterward. A bad npm
  release requires a new version; do not reuse its version number.

The release candidate updates manifests to 0.4.0. Publishing remains gated by
the outstanding manual checks and successful release dry run.
