# Testing and coverage

The test suite covers the Rust shell engine, its JavaScript/WebAssembly
boundary, the browser-facing package, and the deployed demo artifact at
different layers.

## Automated test scope

- `cargo test --workspace` exercises the native Rust engine and CLI, including
  lexing, parsing, expressions, built-in commands, pipelines, rendering,
  cancellation, line editing, sessions, panes, and multiplexer layout.
- `just test-wasm` runs the `wasm-bindgen` boundary suite under Node. It checks
  Rust/JavaScript value conversion, registered JavaScript commands and regexes,
  progressive output, errors, cancellation, variables, and the public `run()`
  result shape.
- `just typecheck` builds the WebAssembly package and type-checks the vanilla
  TypeScript demo against the generated declarations.
- `just test-e2e` runs Playwright against the demo in Chromium. It exercises
  structured pipelines, selectors, diagnostics and escape handling, panel and
  session behavior, streaming, cancellation, host/session variables, terminal
  appearance, and keyboard focus with both panel and custom mounts.
- `scripts/verify-site.mjs` boots the assembled GitHub Pages artifact, while
  `scripts/verify-tarball.mjs` installs and boots the packed npm artifact as an
  external consumer would.

The manual browser checklist lives in
[`packages/demo/TESTING.md`](../packages/demo/TESTING.md).

## Coverage status

The project does not currently publish a numeric line or branch coverage
percentage. CI reports the pass/fail result for each boundary above, so this
document records what is exercised without claiming an unmeasured percentage.
Adding a coverage collector and a stable baseline remains separate work; until
then, changes should add focused tests in the layer whose behavior they alter.

## Local validation

Run the repository's standard validation before opening a pull request:

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features
cargo test --all-features
```

For the WebAssembly and browser boundaries, install the development
prerequisites from the root README and run `just test-wasm` and
`just test-e2e`.

## Filesystem coverage

`packages/demo/tests/default-filesystem.spec.ts` checks automatic writable OPFS
startup without host commands, file reads and redirects, editor cleanup,
session working directories, persistence across terminal recreation, warnings
in both the terminal and console for missing or rejected storage, and the
`filesystem: false` opt-out.

`just test-filesystem` builds the package and runs deterministic Node contract
checks against a memory filesystem, including failure injection, path handling,
permissions, cancellation, transaction rollback, overlapping mounts, editor
conflicts, and experimental devices. After an existing build, run
`npm --prefix packages/browser-terminal run test:filesystem` directly.

The same command runs the log-level filter's Node tests. The wasm boundary
suite checks that the core sends its log lines to the host as `log` events
rather than writing to the console, and the smoke suite covers the default,
`'silent'`, `'debug'`, custom-logger, cleanup-failure, and wrong-MIME-type
behaviour in a browser.

`packages/demo/tests/filesystem.spec.ts` exercises real OPFS through the WASM
shell and editor in Chromium. The CI and release workflows now require these
checks and the complete browser suite, alongside fmt, Clippy, native tests,
and WASM boundary tests. Native folder-picker/save checks remain manual.
